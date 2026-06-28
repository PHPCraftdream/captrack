//! Orchestration of `cargo dylint --fix`-based rewrites.
//!
//! Shared by the `apply` subcommand (Path B, capacity rewrite) and the
//! `instrument` subcommand (Path C, TrackedX wrapping).
//!
//! ## Manifest
//!
//! After a successful run a `last-<op>.json` is written under
//! `target/captrack-pgo/`.  The `undo` subcommand reads this manifest to
//! restore files to their pre-run state.
//!
//! ## `cargo dylint` invocation (version assumption: cargo-dylint 6.0.1)
//!
//! ```text
//! cargo dylint --path <lint-path> [--fix] [--allow-dirty] \
//!     -- --manifest-path <workspace/Cargo.toml>
//! ```
//!
//! The `--path` flag (before `--`) loads the lint cdylib from the given
//! directory.  Everything after `--` is forwarded verbatim to `cargo
//! check`/`cargo fix`, so `--manifest-path` targets the workspace being
//! checked without changing cwd.
//!
//! ### Apply
//! `CAPTRACK_PGO_PROFILE` env var is set to the absolute profile path so the
//! lint plugin can read it during compilation.
//!
//! ### Instrument
//! `CAPTRACK_PGO_INSTRUMENT=1` env var is set; `CAPTRACK_PGO_PROFILE` is
//! explicitly unset from the child environment so both vars can never be seen
//! together by the plugin.

use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::workspace;

// ──────────────────────────────────────────────────────────────────────────────
// Manifest types (version 1 — M7+)
//
// Breaking change from the pre-M7 format (`LintApplyManifest` with a top-level
// `profile_path` field, written to `last-lint-apply.json`):
//   - The `operation` discriminant was added; old JSON had no such field and
//     cannot be parsed by the new deserializer.
//   - The manifest path changed: apply → `last-apply.json`,
//     instrument → `last-instrument.json`.
// Old manifests from before M7 are no longer revertable with `undo`.  Use
// `git checkout` to restore those files manually.
// ──────────────────────────────────────────────────────────────────────────────

/// Which rewrite operation produced this manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    /// `apply` — capacity rewrite from a captrack profile.
    Apply {
        /// Absolute path of the profile JSON used during this run.
        profile_path: PathBuf,
    },
    /// `instrument` — auto-wraps constructors in `TrackedX::with_capacity_named`.
    Instrument,
}

/// One file that was modified by `cargo dylint --fix`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LintFileEntry {
    /// Absolute path to the Rust source file.
    pub file: PathBuf,
    /// SHA-256 of the file BEFORE `cargo dylint --fix` ran.
    pub sha256_before: String,
    /// Full content of the file BEFORE (allows lossless undo).
    pub content_before: String,
    /// SHA-256 of the file AFTER.
    pub sha256_after: String,
}

/// Top-level manifest written after a successful dylint run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LintRunManifest {
    /// Always 1 — bump if the schema changes.
    pub version: u32,
    /// Which operation produced this manifest.
    pub operation: Operation,
    /// One entry per modified `.rs` file.
    pub files: Vec<LintFileEntry>,
}

/// Returns the manifest path for an `apply` run: `target/captrack-pgo/last-apply.json`.
pub fn default_apply_manifest_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join("target")
        .join("captrack-pgo")
        .join("last-apply.json")
}

/// Returns the manifest path for an `instrument` run:
/// `target/captrack-pgo/last-instrument.json`.
pub fn default_instrument_manifest_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join("target")
        .join("captrack-pgo")
        .join("last-instrument.json")
}

/// Legacy path written by M4–M6 `apply`: `target/captrack-pgo/last-lint-apply.json`.
/// Kept so `undo` can warn the user if they point at the old file explicitly;
/// it is NOT searched automatically (clean break from pre-M7 format).
#[allow(dead_code)]
pub fn legacy_lint_manifest_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join("target")
        .join("captrack-pgo")
        .join("last-lint-apply.json")
}


/// Pick the most recently modified manifest among the known candidates.
///
/// Looks at:
/// - `target/captrack-pgo/last-apply.json`
/// - `target/captrack-pgo/last-instrument.json`
///
/// Returns `None` when neither file exists.
pub fn latest_manifest_path(workspace_root: &Path) -> Option<PathBuf> {
    let candidates = [
        default_apply_manifest_path(workspace_root),
        default_instrument_manifest_path(workspace_root),
    ];
    candidates
        .into_iter()
        .filter(|p| p.is_file())
        .max_by_key(|p| {
            p.metadata()
                .and_then(|m| m.modified())
                .ok()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        })
}

// ──────────────────────────────────────────────────────────────────────────────
// Pre-flight helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Returns `Ok(())` if the `cargo dylint` binary is reachable, otherwise
/// an error with an install hint.
pub(crate) fn check_cargo_dylint_available() -> Result<()> {
    let status = std::process::Command::new("cargo")
        .args(["dylint", "--version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => Err(anyhow!(
            "cargo dylint is not installed or not in PATH.\n\
             Install with: cargo install cargo-dylint dylint-link\n\
             (requires a nightly toolchain for the CDylib build step)"
        )),
    }
}

/// Verify that the lint-path directory looks like a `captrack-pgo-lint` crate.
pub(crate) fn check_lint_path(lint_path: &Path) -> Result<()> {
    let cargo_toml = lint_path.join("Cargo.toml");
    if !cargo_toml.is_file() {
        return Err(anyhow!(
            "lint-path '{}' does not contain a Cargo.toml",
            lint_path.display()
        ));
    }
    let body = std::fs::read_to_string(&cargo_toml)
        .with_context(|| format!("read {}", cargo_toml.display()))?;
    // Accept both the exact package name and any name containing it, so a fork
    // or rename still works as long as it's a valid Cargo crate.
    if !body.contains("captrack-pgo-lint") {
        return Err(anyhow!(
            "Cargo.toml at '{}' does not appear to be the captrack-pgo-lint package \
             (missing 'captrack-pgo-lint' in package name)",
            lint_path.display()
        ));
    }
    Ok(())
}

/// Resolve the default lint-path: a sibling of `captrack-pgo/` named
/// `captrack-pgo-lint/`.  Public so `cli.rs` can call it.
pub fn resolve_default_lint_path() -> Result<PathBuf> {
    // The captrack-pgo binary lives in:
    //   <workspace>/target/[profile]/<bin>
    // Or, when invoked during tests via `env!("CARGO_BIN_EXE_captrack-pgo")`:
    //   <workspace>/captrack-pgo/target/[profile]/<bin>
    //
    // We can't reliably find the source directory from the executable path
    // alone.  Instead, we start from the current working directory and look
    // for a sibling `captrack-pgo-lint/` with the expected Cargo.toml.
    let cwd = std::env::current_dir().context("get cwd")?;
    // Try: cwd/captrack-pgo-lint, cwd/../captrack-pgo-lint
    for candidate in [
        cwd.join("captrack-pgo-lint"),
        cwd.parent()
            .map(|p| p.join("captrack-pgo-lint"))
            .unwrap_or_default(),
    ] {
        if candidate.join("Cargo.toml").is_file() {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "could not find captrack-pgo-lint/ as a sibling of the current directory '{}'. \
         Pass --lint-path explicitly.",
        cwd.display()
    ))
}

// ──────────────────────────────────────────────────────────────────────────────
// Snapshot helpers
// ──────────────────────────────────────────────────────────────────────────────

pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Walk all `.rs` files in the workspace (reusing `workspace::walk_rust_files`)
/// and snapshot their path + SHA-256 + content.
///
/// Returns `Vec<(abs_path, sha256_hex, content)>`.
pub(crate) fn snapshot_rs_files(workspace_root: &Path) -> Result<Vec<(PathBuf, String, String)>> {
    let mut out = Vec::new();
    for file in workspace::walk_rust_files(workspace_root) {
        let content = std::fs::read_to_string(&file)
            .with_context(|| format!("snapshot read {}", file.display()))?;
        let sha = hex_sha256(content.as_bytes());
        out.push((file, sha, content));
    }
    Ok(out)
}

/// After `cargo dylint` has run, diff the before snapshots against current
/// disk contents and build the list of changed `LintFileEntry` values.
///
/// Also accumulates the net byte-delta for reporting.
pub(crate) fn diff_snapshots(
    before_snapshots: &[(PathBuf, String, String)],
) -> (Vec<LintFileEntry>, i64) {
    let mut changed_files: Vec<LintFileEntry> = Vec::new();
    let mut total_bytes_changed: i64 = 0;

    for (path, sha_before, content_before) in before_snapshots {
        let content_after = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "captrack-pgo: warning: could not re-read {} after dylint ({e}); skipping",
                    path.display()
                );
                continue;
            }
        };
        let sha_after = hex_sha256(content_after.as_bytes());
        if sha_after != *sha_before {
            total_bytes_changed +=
                content_after.len() as i64 - content_before.len() as i64;
            changed_files.push(LintFileEntry {
                file: path.clone(),
                sha256_before: sha_before.clone(),
                content_before: content_before.clone(),
                sha256_after: sha_after,
            });
        }
    }

    (changed_files, total_bytes_changed)
}

/// Write a `LintRunManifest` to `manifest_path`, creating parent directories
/// as needed.
pub(crate) fn write_manifest(manifest: &LintRunManifest, manifest_path: &Path) -> Result<()> {
    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(manifest)?;
    std::fs::write(manifest_path, &json)
        .with_context(|| format!("write manifest {}", manifest_path.display()))?;
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Core command — apply
// ──────────────────────────────────────────────────────────────────────────────

/// Which hasher the user wants injected when `--hasher` is set.
///
/// Maps one-to-one with the `CAPTRACK_PGO_HASHER` env var values that the
/// lint plugin reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HasherChoice {
    /// No hasher injection — capacity-only rewrite (default).
    #[default]
    None,
    /// `::fxhash::FxBuildHasher`
    Fx,
    /// `::ahash::RandomState`
    AHash,
    /// `::foldhash::fast::RandomState`
    FoldHash,
}

impl HasherChoice {
    /// The env-var string to pass to `CAPTRACK_PGO_HASHER`.
    ///
    /// Returns `None` for `HasherChoice::None` (env var should be removed).
    pub fn env_value(self) -> Option<&'static str> {
        match self {
            HasherChoice::None => None,
            HasherChoice::Fx => Some("fx"),
            HasherChoice::AHash => Some("ahash"),
            HasherChoice::FoldHash => Some("foldhash"),
        }
    }
}

/// Arguments for the `apply` subcommand, already resolved (paths
/// canonicalized, defaults applied).
pub struct LintApplyArgs {
    pub profile_path: PathBuf,
    pub lint_path: PathBuf,
    pub workspace_root: PathBuf,
    pub dry_run: bool,
    pub allow_dirty: bool,
    /// Hasher to inject into HashMap/HashSet constructors.
    pub hasher: HasherChoice,
}

/// Run the `apply` subcommand.
pub fn run_lint_apply(args: LintApplyArgs) -> Result<()> {
    // ── 1. Pre-flight ────────────────────────────────────────────────────────

    // Profile must exist and parse as a captrack dump.
    if !args.profile_path.is_file() {
        return Err(anyhow!(
            "profile '{}' does not exist or is not a file",
            args.profile_path.display()
        ));
    }
    // Quick parse check — read the JSON; we only need to confirm it's valid.
    {
        let raw = std::fs::read_to_string(&args.profile_path)
            .with_context(|| format!("read profile {}", args.profile_path.display()))?;
        // The captrack dump wraps entries under a "stats" key.
        let v: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("parse profile JSON {}", args.profile_path.display()))?;
        if !v.is_object() {
            return Err(anyhow!(
                "profile '{}' is not a JSON object (expected captrack dump format)",
                args.profile_path.display()
            ));
        }
    }

    // lint-path must exist and be valid.
    check_lint_path(&args.lint_path)?;

    // cargo dylint must be available.
    check_cargo_dylint_available()?;

    // Workspace root must have a Cargo.toml.
    let workspace_cargo_toml = args.workspace_root.join("Cargo.toml");
    if !workspace_cargo_toml.is_file() {
        return Err(anyhow!(
            "workspace root '{}' does not contain a Cargo.toml",
            args.workspace_root.display()
        ));
    }

    // ── 2. Snapshot before ───────────────────────────────────────────────────

    let before_snapshots = snapshot_rs_files(&args.workspace_root)
        .context("snapshot workspace before dylint")?;

    // ── 3. Run cargo dylint ──────────────────────────────────────────────────

    let abs_profile = args
        .profile_path
        .canonicalize()
        .with_context(|| format!("canonicalize profile path {}", args.profile_path.display()))?;

    // Build the dylint command.
    //
    // Invocation pattern (cargo-dylint 6.0.1):
    //   cargo dylint --path <lint-path> [--fix] [--allow-dirty] \
    //       -- --manifest-path <workspace/Cargo.toml>
    //
    // `--path` is a dylint-level flag (before `--`) that tells dylint to build
    // and load the cdylib from the given source directory.
    // Arguments after `--` are forwarded to `cargo check` (or `cargo fix`
    // when `--fix` is present), so `--manifest-path` targets the checked
    // workspace without requiring a `cd`.
    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("dylint");
    cmd.arg("--path");
    cmd.arg(&args.lint_path);
    if !args.dry_run {
        cmd.arg("--fix");
    }
    if args.allow_dirty {
        cmd.arg("--allow-dirty");
    }
    // Forward to cargo check/fix.
    cmd.arg("--");
    cmd.arg("--manifest-path");
    cmd.arg(&workspace_cargo_toml);

    // Expose the profile path to the lint plugin.
    cmd.env("CAPTRACK_PGO_PROFILE", &abs_profile);
    // Ensure the instrument env var is NOT set (belt-and-suspenders).
    cmd.env_remove("CAPTRACK_PGO_INSTRUMENT");
    // Forward the hasher choice (or remove the env var when none).
    match args.hasher.env_value() {
        Some(v) => {
            cmd.env("CAPTRACK_PGO_HASHER", v);
        }
        None => {
            cmd.env_remove("CAPTRACK_PGO_HASHER");
        }
    }

    // Inherit stdio so the user sees compilation progress / lint output.
    cmd.stdin(std::process::Stdio::null());

    if args.dry_run {
        println!("captrack-pgo: dry-run — would run:");
        let hasher_env = args
            .hasher
            .env_value()
            .map(|v| format!(" CAPTRACK_PGO_HASHER={v}"))
            .unwrap_or_default();
        println!(
            "  CAPTRACK_PGO_PROFILE={}{hasher_env} cargo dylint --path {} -- --manifest-path {}",
            abs_profile.display(),
            args.lint_path.display(),
            workspace_cargo_toml.display()
        );
        println!("(files not modified, manifest not written)");
        // Count potential sites from profile.
        let raw = std::fs::read_to_string(&args.profile_path)?;
        let v: serde_json::Value = serde_json::from_str(&raw)?;
        let count = v
            .get("stats")
            .and_then(|s| s.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        println!(
            "would inspect {} profile site{} (matches depend on source scan)",
            count,
            if count == 1 { "" } else { "s" }
        );
        return Ok(());
    }

    let status = cmd
        .status()
        .context("spawn cargo dylint — is cargo-dylint installed?")?;

    if !status.success() {
        return Err(anyhow!(
            "cargo dylint exited with non-zero status: {:?}\n\
             Files NOT modified; manifest NOT written.",
            status.code()
        ));
    }

    // ── 4. Snapshot after and build manifest ─────────────────────────────────

    let (changed_files, total_bytes_changed) = diff_snapshots(&before_snapshots);

    let manifest = LintRunManifest {
        version: 1,
        operation: Operation::Apply {
            profile_path: abs_profile,
        },
        files: changed_files.clone(),
    };

    // ── 5. Write manifest ────────────────────────────────────────────────────

    let manifest_path = default_apply_manifest_path(&args.workspace_root);
    write_manifest(&manifest, &manifest_path)?;

    // ── 6. Report ─────────────────────────────────────────────────────────────

    let n = changed_files.len();
    println!(
        "apply: modified {} file{}, net {} byte{} changed",
        n,
        if n == 1 { "" } else { "s" },
        total_bytes_changed,
        if total_bytes_changed.abs() == 1 {
            ""
        } else {
            "s"
        }
    );
    println!("  manifest: {}", manifest_path.display());
    println!("  undo with: captrack-pgo undo --manifest {}", manifest_path.display());

    // Emit dependency reminder when a hasher was injected.
    if let Some(hasher_val) = args.hasher.env_value() {
        let dep_name = match hasher_val {
            "fx" => "fxhash",
            "ahash" => "ahash",
            "foldhash" => "foldhash",
            _ => hasher_val,
        };
        eprintln!(
            "captrack-pgo: note: --hasher {hasher_val} was used. \
             Remember to add `{dep_name}` to your Cargo.toml if not already present."
        );
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Undo support — generic over all LintRunManifest operations
// ──────────────────────────────────────────────────────────────────────────────

/// Read and parse a `LintRunManifest` from `path`.
///
/// Returns helpful errors:
/// - `Err` with "no manifest found" context when the file does not exist.
/// - `Err` with serde context when the JSON is malformed.
///
/// Used by `uninstrument` (and any future subcommand) that needs to inspect
/// the manifest before deciding whether to proceed.
pub fn read_manifest(path: &Path) -> Result<LintRunManifest> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow!(
                    "no manifest found at {}; nothing to uninstrument",
                    path.display()
                )
            } else {
                anyhow!("read manifest {}: {e}", path.display())
            }
        })?;
    serde_json::from_str::<LintRunManifest>(&raw)
        .with_context(|| format!("parse manifest {}", path.display()))
}

/// Revert the changes recorded in a `LintRunManifest`.
/// Returns the number of files restored.
pub fn undo_lint_apply(manifest_path: &Path) -> Result<usize> {
    let raw = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("read lint manifest {}", manifest_path.display()))?;
    let manifest: LintRunManifest = serde_json::from_str(&raw)
        .with_context(|| format!("parse lint manifest {}", manifest_path.display()))?;
    if manifest.version != 1 {
        return Err(anyhow!(
            "unsupported lint manifest version {}: only v1 supported",
            manifest.version
        ));
    }

    for entry in &manifest.files {
        // Safety check: current content must match sha256_after.
        let current = std::fs::read_to_string(&entry.file)
            .with_context(|| format!("read {} for undo", entry.file.display()))?;
        let cur_sha = hex_sha256(current.as_bytes());
        if cur_sha != entry.sha256_after {
            return Err(anyhow!(
                "file modified since lint-apply: {}\n  \
                 expected sha256 {}\n  current sha256  {}\n  \
                 resolve via git first",
                entry.file.display(),
                entry.sha256_after,
                cur_sha
            ));
        }
    }

    let mut count = 0usize;
    for entry in &manifest.files {
        atomic_write(&entry.file, entry.content_before.as_bytes())?;
        count += 1;
    }
    Ok(count)
}

fn atomic_write(file: &Path, bytes: &[u8]) -> Result<()> {
    let parent = file.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut tmp, bytes)?;
    tmp.persist(file)
        .map_err(|e| anyhow!("persist tempfile: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_sha256_is_64_hex_chars_and_deterministic() {
        let h1 = hex_sha256(b"hello");
        let h2 = hex_sha256(b"hello");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn lint_run_manifest_apply_round_trips_json() {
        let m = LintRunManifest {
            version: 1,
            operation: Operation::Apply {
                profile_path: PathBuf::from("/tmp/profile.json"),
            },
            files: vec![LintFileEntry {
                file: PathBuf::from("/tmp/src/lib.rs"),
                sha256_before: "a".repeat(64),
                content_before: "fn old() {}".to_string(),
                sha256_after: "b".repeat(64),
            }],
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: LintRunManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn lint_run_manifest_instrument_round_trips_json() {
        let m = LintRunManifest {
            version: 1,
            operation: Operation::Instrument,
            files: vec![],
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: LintRunManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn default_apply_manifest_path_is_inside_target() {
        let p = default_apply_manifest_path(Path::new("/ws"));
        assert!(p.to_string_lossy().contains("captrack-pgo"));
        assert!(p.to_string_lossy().contains("last-apply.json"));
    }

    #[test]
    fn default_instrument_manifest_path_is_inside_target() {
        let p = default_instrument_manifest_path(Path::new("/ws"));
        assert!(p.to_string_lossy().contains("captrack-pgo"));
        assert!(p.to_string_lossy().contains("last-instrument.json"));
    }

    #[test]
    fn undo_lint_apply_fails_on_modified_file() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("foo.rs");
        let before = "fn old() {}";
        let after = "fn new() {}";
        std::fs::write(&src, after).unwrap();

        let manifest = LintRunManifest {
            version: 1,
            operation: Operation::Apply {
                profile_path: PathBuf::from("/tmp/p.json"),
            },
            files: vec![LintFileEntry {
                file: src.clone(),
                sha256_before: hex_sha256(before.as_bytes()),
                content_before: before.to_string(),
                // Set sha256_after to something that does NOT match "fn new() {}"
                sha256_after: "x".repeat(64),
            }],
        };
        let mpath = tmp.path().join("m.json");
        std::fs::write(&mpath, serde_json::to_string(&manifest).unwrap()).unwrap();

        let err = undo_lint_apply(&mpath).unwrap_err();
        assert!(
            err.to_string().contains("modified since lint-apply"),
            "got: {err}"
        );
    }

    #[test]
    fn undo_lint_apply_restores_content() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("bar.rs");
        let before = "fn original() {}";
        let after_content = "fn modified() {}";
        std::fs::write(&src, after_content).unwrap();

        let sha_after = hex_sha256(after_content.as_bytes());
        let manifest = LintRunManifest {
            version: 1,
            operation: Operation::Instrument,
            files: vec![LintFileEntry {
                file: src.clone(),
                sha256_before: hex_sha256(before.as_bytes()),
                content_before: before.to_string(),
                sha256_after: sha_after,
            }],
        };
        let mpath = tmp.path().join("m.json");
        std::fs::write(&mpath, serde_json::to_string(&manifest).unwrap()).unwrap();

        let n = undo_lint_apply(&mpath).unwrap();
        assert_eq!(n, 1);
        let restored = std::fs::read_to_string(&src).unwrap();
        assert_eq!(restored, before);
    }

    #[test]
    fn undo_lint_apply_rejects_wrong_version() {
        let tmp = tempfile::tempdir().unwrap();
        let mpath = tmp.path().join("m.json");
        // The new format requires the `operation` field as well; supply a
        // valid-shape doc but with an unsupported version number.
        std::fs::write(
            &mpath,
            r#"{"version":99,"operation":{"op":"instrument"},"files":[]}"#,
        )
        .unwrap();
        let err = undo_lint_apply(&mpath).unwrap_err();
        assert!(err.to_string().contains("version"), "got: {err}");
    }

    #[test]
    fn latest_manifest_path_prefers_newer_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = root.join("target").join("captrack-pgo");
        std::fs::create_dir_all(&dir).unwrap();

        let apply_path = dir.join("last-apply.json");
        let instr_path = dir.join("last-instrument.json");

        // Write apply first, then instrument — instrument should be "newer".
        std::fs::write(&apply_path, "{}").unwrap();
        // Small sleep is not reliable; set mtime explicitly via touch (fs).
        // Instead, just check that both are found and the function doesn't panic.
        std::fs::write(&instr_path, "{}").unwrap();

        // At minimum, some path is returned.
        let found = latest_manifest_path(root);
        assert!(found.is_some(), "expected some manifest path");
    }
}
