//! `instrument` subcommand — orchestrates `cargo dylint --fix` with
//! `CAPTRACK_PGO_INSTRUMENT=1` to auto-wrap bare std collection constructors
//! in `::captrack::TrackedX::with_capacity_named(...)`.
//!
//! ## What it does
//!
//! 1. Pre-flight checks (cargo-dylint available, lint-path valid, workspace
//!    Cargo.toml exists, captrack dep + telemetry feature warnings).
//! 2. Snapshot all `.rs` files in the workspace before the run.
//! 3. Run:
//!    ```text
//!    CAPTRACK_PGO_INSTRUMENT=1 cargo dylint --path <lint-path> [--fix] \
//!        [--allow-dirty] -- --manifest-path <workspace/Cargo.toml>
//!    ```
//!    `CAPTRACK_PGO_PROFILE` is explicitly removed from the child env.
//! 4. Diff before/after snapshots, write `target/captrack-pgo/last-instrument.json`.
//!
//! `--dry-run` runs `cargo dylint` WITHOUT `--fix` (suggestions appear in
//! stderr as warnings; files are not modified; no manifest is written).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::lint_apply::{
    check_cargo_dylint_available, check_lint_path, default_instrument_manifest_path,
    diff_snapshots, resolve_default_lint_path, snapshot_rs_files, write_manifest, LintRunManifest,
    Operation,
};

// ──────────────────────────────────────────────────────────────────────────────
// Public arguments struct
// ──────────────────────────────────────────────────────────────────────────────

/// Arguments for the `instrument` subcommand, already resolved (paths
/// canonicalized, defaults applied).
pub struct LintInstrumentArgs {
    pub lint_path: PathBuf,
    pub workspace_root: PathBuf,
    pub dry_run: bool,
    pub allow_dirty: bool,
}

// ──────────────────────────────────────────────────────────────────────────────
// captrack-dep pre-flight warnings
// ──────────────────────────────────────────────────────────────────────────────

/// Check whether the workspace has `captrack` as a dependency and whether the
/// `telemetry` feature is enabled.
///
/// Emits warnings (to stderr) when:
/// - `captrack` is not listed as a dependency in any member crate — the
///   instrumented code calls `::captrack::TrackedX::with_capacity_named(...)`,
///   which won't compile without it.
/// - `captrack` is a dep but its `telemetry` feature isn't enabled — the
///   instrumented code will compile but record nothing.
///
/// Both warnings are non-fatal; the subcommand proceeds.
pub fn warn_if_captrack_dep_missing_or_telemetry_off(workspace_root: &Path) {
    let root_toml = workspace_root.join("Cargo.toml");
    if !root_toml.is_file() {
        return; // Already checked earlier; silently skip.
    }

    let root_body = match std::fs::read_to_string(&root_toml) {
        Ok(b) => b,
        Err(_) => return,
    };

    // Collect all Cargo.toml paths to inspect: the workspace root plus every
    // member crate.  We do a lightweight parse of the `members = [...]` list
    // rather than pulling in a full TOML parser.
    let member_tomls = collect_member_tomls(workspace_root, &root_body);

    let mut found_captrack = false;
    let mut found_telemetry = false;

    for toml_path in &member_tomls {
        let body = match std::fs::read_to_string(toml_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if has_captrack_dep(&body) {
            found_captrack = true;
            if has_telemetry_feature(&body) {
                found_telemetry = true;
            }
        }
    }

    // Also check the workspace-level [dependencies] table (uncommon but valid).
    if has_captrack_dep(&root_body) {
        found_captrack = true;
        if has_telemetry_feature(&root_body) {
            found_telemetry = true;
        }
    }

    if !found_captrack {
        eprintln!(
            "captrack-pgo: warning: workspace does not appear to depend on `captrack`.\n\
             Instrument will rewrite constructors to `::captrack::TrackedX::with_capacity_named(...)`\n\
             which won't compile unless `captrack` is added as a dependency.\n\
             Add to your Cargo.toml: captrack = {{ version = \"*\", features = [\"telemetry\"] }}"
        );
    } else if !found_telemetry {
        eprintln!(
            "captrack-pgo: warning: `captrack` is a dependency but the `telemetry` feature \
             does not appear to be enabled.\n\
             The instrumented code will compile but record nothing.\n\
             Enable with: captrack = {{ version = \"*\", features = [\"telemetry\"] }}"
        );
    }
}

/// Collect Cargo.toml paths for the workspace root + all declared members.
fn collect_member_tomls(workspace_root: &Path, root_toml_body: &str) -> Vec<PathBuf> {
    let mut paths = vec![workspace_root.join("Cargo.toml")];

    // Extract the `members = [...]` array with a lightweight line-by-line scan.
    // Handles the common case of `members = ["a", "b/c"]` on one or multiple
    // lines within a `[workspace]` section.
    let mut in_workspace_section = false;
    let mut in_members_array = false;

    for line in root_toml_body.lines() {
        let trimmed = line.trim();

        // Detect section headers.
        if trimmed.starts_with('[') {
            in_workspace_section = trimmed == "[workspace]" || trimmed.starts_with("[workspace]");
            in_members_array = false; // reset when entering any new section
            continue;
        }

        if !in_workspace_section {
            continue;
        }

        // Detect the `members` key.
        if trimmed.starts_with("members") || in_members_array {
            in_members_array = true;
            // Extract all quoted strings from this line.
            let mut pos = 0;
            let line_bytes = trimmed.as_bytes();
            while pos < line_bytes.len() {
                if line_bytes[pos] == b'"' {
                    let start = pos + 1;
                    if let Some(end_off) = trimmed[start..].find('"') {
                        let member = &trimmed[start..start + end_off];
                        let member_toml = workspace_root.join(member).join("Cargo.toml");
                        paths.push(member_toml);
                        pos = start + end_off + 1;
                    } else {
                        break;
                    }
                } else {
                    pos += 1;
                }
            }
            // The array ends when we see `]` without a matching open `[`
            // (this is a very rough heuristic but handles common formatting).
            if trimmed.contains(']') {
                in_members_array = false;
            }
        }
    }

    paths
}

/// Returns `true` if the TOML body contains a `captrack` dependency entry.
///
/// Matches lines like:
/// - `captrack = ...`
/// - `captrack = { version = ...`
fn has_captrack_dep(body: &str) -> bool {
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            continue;
        }
        // Match `captrack = ...` (dependency declaration).
        if t.starts_with("captrack") {
            let rest = t.trim_start_matches("captrack").trim_start();
            if rest.starts_with('=') {
                return true;
            }
        }
    }
    false
}

/// Returns `true` if any dependency entry for `captrack` mentions
/// `features = [... "telemetry" ...]` or `features = ["telemetry", ...]`
/// (same-line inline check).
fn has_telemetry_feature(body: &str) -> bool {
    let mut in_captrack_block = false;

    for line in body.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            continue;
        }

        // Detect the start of a captrack dependency section.
        if t.starts_with("captrack") {
            let rest = t.trim_start_matches("captrack").trim_start();
            if rest.starts_with('=') {
                in_captrack_block = true;
            }
        }

        if in_captrack_block {
            if t.contains("telemetry") {
                return true;
            }
            // End of the inline value (no continuation into next block).
            // A simple heuristic: if the line contains `}` we're done with
            // inline table; if it's a new `[section]` or another key = value
            // that isn't captrack, we stop.
            if !t.starts_with("captrack") && (t.contains('}') || (t.contains('=') && !t.is_empty())) {
                in_captrack_block = false;
            }
        }
    }
    false
}

// ──────────────────────────────────────────────────────────────────────────────
// Core command
// ──────────────────────────────────────────────────────────────────────────────

/// Run the `instrument` subcommand.
pub fn run_lint_instrument(args: LintInstrumentArgs) -> Result<()> {
    // ── 1. Pre-flight ────────────────────────────────────────────────────────

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

    // Warn (non-fatal) if captrack dep or telemetry feature is missing.
    warn_if_captrack_dep_missing_or_telemetry_off(&args.workspace_root);

    // ── 2. Snapshot before ───────────────────────────────────────────────────

    let before_snapshots = snapshot_rs_files(&args.workspace_root)
        .context("snapshot workspace before dylint")?;

    // ── 3. Build and (optionally) run cargo dylint ───────────────────────────
    //
    // Invocation:
    //   CAPTRACK_PGO_INSTRUMENT=1 cargo dylint --path <lint-path> [--fix]
    //       [--allow-dirty] -- --manifest-path <workspace/Cargo.toml>
    //
    // `CAPTRACK_PGO_PROFILE` is explicitly removed so the plugin never sees
    // both env vars at once.
    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("dylint");
    cmd.arg("--path");
    cmd.arg(&args.lint_path);
    if !args.dry_run {
        cmd.arg("--fix");
    }
    cmd.arg("--");
    cmd.arg("--manifest-path");
    cmd.arg(&workspace_cargo_toml);
    // `--allow-dirty` is a cargo-fix flag (consumed by the inner `cargo fix`
    // invocation, not by `cargo dylint`), so it MUST follow `--`.
    if args.allow_dirty {
        cmd.arg("--allow-dirty");
    }

    // Instrument env vars.
    cmd.env("CAPTRACK_PGO_INSTRUMENT", "1");
    cmd.env_remove("CAPTRACK_PGO_PROFILE");

    // Inherit stdio so the user sees warnings / progress.
    cmd.stdin(std::process::Stdio::null());

    if args.dry_run {
        // In dry-run mode we still run cargo dylint but WITHOUT `--fix`.
        // This causes the lint to emit warnings to stderr — useful for "what
        // would change" — without touching any files.
        println!("captrack-pgo: dry-run — running without --fix (suggestions appear in stderr):");
        println!(
            "  CAPTRACK_PGO_INSTRUMENT=1 cargo dylint --path {} -- --manifest-path {}",
            args.lint_path.display(),
            workspace_cargo_toml.display()
        );
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

    // In dry-run mode: files are unchanged (no --fix), no manifest written.
    if args.dry_run {
        println!("(dry-run complete — no files modified, manifest not written)");
        return Ok(());
    }

    // ── 4. Snapshot after and build manifest ─────────────────────────────────

    let (changed_files, total_bytes_changed) = diff_snapshots(&before_snapshots);

    let manifest = LintRunManifest {
        version: 1,
        operation: Operation::Instrument,
        files: changed_files.clone(),
    };

    // ── 5. Write manifest ────────────────────────────────────────────────────

    let manifest_path = default_instrument_manifest_path(&args.workspace_root);
    write_manifest(&manifest, &manifest_path)?;

    // ── 6. Report ─────────────────────────────────────────────────────────────

    let n = changed_files.len();
    println!(
        "instrument: modified {} file{}, net {} byte{} changed",
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
    println!(
        "  revert with: captrack-pgo undo --manifest {}",
        manifest_path.display()
    );

    Ok(())
}

/// Resolve the `--lint-path` argument for the `instrument` subcommand.
/// Delegates to the shared resolver from `lint_apply`.
pub fn resolve_lint_path(lint_path: Option<PathBuf>) -> Result<PathBuf> {
    match lint_path {
        Some(p) => Ok(p),
        None => resolve_default_lint_path(),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Unit tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_captrack_dep_detects_simple_dep() {
        let toml = r#"
[dependencies]
captrack = { version = "0.1", features = ["telemetry"] }
anyhow = "1"
"#;
        assert!(has_captrack_dep(toml));
    }

    #[test]
    fn has_captrack_dep_false_for_no_dep() {
        let toml = r#"
[dependencies]
anyhow = "1"
serde = "1"
"#;
        assert!(!has_captrack_dep(toml));
    }

    #[test]
    fn has_captrack_dep_ignores_comments() {
        let toml = r#"
[dependencies]
# captrack = "0.1"
anyhow = "1"
"#;
        assert!(!has_captrack_dep(toml));
    }

    #[test]
    fn has_telemetry_feature_inline() {
        let toml = r#"
[dependencies]
captrack = { version = "0.1", features = ["telemetry"] }
"#;
        assert!(has_captrack_dep(toml));
        assert!(has_telemetry_feature(toml));
    }

    #[test]
    fn has_telemetry_feature_missing() {
        let toml = r#"
[dependencies]
captrack = { version = "0.1" }
"#;
        assert!(has_captrack_dep(toml));
        assert!(!has_telemetry_feature(toml));
    }

    #[test]
    fn collect_member_tomls_finds_members() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let toml_body = "[workspace]\nmembers = [\"alpha\", \"beta\"]\n";
        std::fs::create_dir_all(root.join("alpha")).unwrap();
        std::fs::create_dir_all(root.join("beta")).unwrap();
        std::fs::write(root.join("alpha").join("Cargo.toml"), "").unwrap();
        std::fs::write(root.join("beta").join("Cargo.toml"), "").unwrap();

        let paths = collect_member_tomls(root, toml_body);
        // Should include root + alpha + beta
        assert!(paths.len() >= 3, "expected at least 3 paths, got {:?}", paths);
        assert!(paths.iter().any(|p| p.ends_with("alpha/Cargo.toml") || p.ends_with("alpha\\Cargo.toml")));
        assert!(paths.iter().any(|p| p.ends_with("beta/Cargo.toml") || p.ends_with("beta\\Cargo.toml")));
    }

    /// Smoke test: running the captrack-dep warning check on a workspace that
    /// has NO captrack dep must not panic.
    #[test]
    fn warn_check_no_captrack_does_not_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = []\n[dependencies]\nanyhow = \"1\"\n",
        )
        .unwrap();
        // Should emit a warning to stderr but not panic.
        warn_if_captrack_dep_missing_or_telemetry_off(root);
    }

    /// Smoke test: workspace WITH captrack + telemetry must not panic either.
    #[test]
    fn warn_check_with_captrack_telemetry_does_not_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = []\n[dependencies]\ncaptrack = { version = \"0.1\", features = [\"telemetry\"] }\n",
        )
        .unwrap();
        warn_if_captrack_dep_missing_or_telemetry_off(root);
    }
}
