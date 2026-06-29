//! `measure` subcommand — one-command PGO collection orchestration.
//!
//! Executes the full wire → instrument → bench → dump → merge →
//! uninstrument → unwire cycle in a single invocation, with RAII-style
//! cleanup that runs uninstrument + unwire even on error or panic.
//!
//! ## Orchestration steps
//!
//! 1. `wire` the target workspace (inject `captrack` dep).
//! 2. `instrument` the workspace (cargo dylint --fix).
//! 3. For each `--bench <name>`:
//!    a. Build the bench without running: `cargo bench --no-run`.
//!    b. Locate the compiled binary in `target/.../deps/`.
//!    c. Run the binary with `CAPTRACK_DUMP_DIR` set; collect the dump.
//! 4. `merge` all per-bench dumps into `--out merged.json`.
//! 5. `uninstrument` + `unwire` (cleanup, also runs on error via Drop-guard).
//!
//! ## RAII cleanup
//!
//! `CleanupGuard` holds the workspace root and runs `uninstrument` + `unwire`
//! in its `Drop` impl (best-effort: errors are printed to stderr, not re-raised).
//! On the happy path, `CleanupGuard::disarm()` is called **before** the explicit
//! cleanup so the Drop impl is a no-op, preventing double-cleanup.
//!
//! ## Bench-binary discovery
//!
//! After `cargo bench --no-run -p <crate> --bench <name>` we scan
//! `<workspace>/target/<profile>/deps/` for executables whose stem starts with
//! `<name>-` (cargo appends a hash suffix).  On Windows the file must have a
//! `.exe` extension; on other platforms we check the executable bit.
//!
//! ## Cargo metadata
//!
//! Crate-to-bench mapping is resolved by calling
//! `cargo metadata --format-version 1 --no-deps --manifest-path <workspace>/Cargo.toml`
//! and parsing the resulting JSON.  No additional crate dependency is required.
//!
//! ## TODO / followups
//!
//! - **Real integration test**: a true end-to-end test requires cargo + nightly
//!   + the dylint plugin — skipped here, marked `#[ignore]`.
//! - **Staleness-guard**: record file hashes after `instrument` and refuse
//!   `apply` if sources changed before apply.  Useful but separate scope.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

use crate::lint_apply::{self, default_instrument_manifest_path, Operation};
use crate::lint_instrument::{self, LintInstrumentArgs};
use crate::merge::{self, MergeArgs};
use crate::wire::{self, UnwireArgs, WireArgs};
use crate::workspace;

// ──────────────────────────────────────────────────────────────────────────────
// Public API
// ──────────────────────────────────────────────────────────────────────────────

/// All arguments for the `measure` subcommand.
pub struct MeasureArgs {
    /// Workspace root of the target project to measure.
    pub workspace: PathBuf,
    /// Path to a local `captrack` checkout (fed to `wire --captrack-path`).
    pub captrack_path: PathBuf,
    /// Bench target names (at least one required).
    pub benches: Vec<String>,
    /// Path to the `captrack-pgo-lint` crate (fed to `instrument --lint-path`).
    pub lint_path: Option<PathBuf>,
    /// Output path for the merged profile JSON.
    pub out: PathBuf,
    /// Per-bench binary timeout.  The process is killed after this duration.
    pub bench_timeout: Duration,
    /// Override `CARGO_TARGET_DIR` for the target workspace builds.
    pub cargo_target_dir: Option<PathBuf>,
    /// Reservoir cap passed to `merge`.  0 = unlimited.
    pub reservoir_cap: usize,
    /// Pass `--allow-dirty` to cargo dylint.
    pub allow_dirty: bool,
}

/// Summary returned by `run_measure`.
#[derive(Debug)]
pub struct MeasureReport {
    pub benches_run: Vec<BenchReport>,
    pub unique_sites: usize,
    pub merged_path: PathBuf,
}

/// Per-bench result.
#[derive(Debug)]
pub struct BenchReport {
    pub name: String,
    pub profile_path: PathBuf,
    pub exit_code: i32,
}

// ──────────────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────────────

/// Run the full measure cycle; returns a `MeasureReport` on success.
///
/// On any step error, the RAII guard ensures uninstrument + unwire run
/// (best-effort) before the error is propagated.
pub fn run_measure(args: MeasureArgs) -> Result<MeasureReport> {
    // ── Validate ─────────────────────────────────────────────────────────────
    if args.benches.is_empty() {
        bail!("--bench must specify at least one bench target name");
    }
    let workspace_root = workspace::find_workspace_root(&args.workspace)
        .with_context(|| format!("locate workspace root from {}", args.workspace.display()))?;

    // ── Step 1: wire ─────────────────────────────────────────────────────────
    eprintln!(
        "measure: [1/5] wire {} (captrack: {})",
        workspace_root.display(),
        args.captrack_path.display(),
    );
    wire::run_wire(WireArgs {
        workspace_root: workspace_root.clone(),
        captrack_path: Some(args.captrack_path.clone()),
    })
    .context("wire")?;

    // ── RAII guard — disarm on success path ───────────────────────────────────
    let mut guard = CleanupGuard::new(workspace_root.clone());

    // ── Step 2: instrument ───────────────────────────────────────────────────
    eprintln!("measure: [2/5] instrument");
    let resolved_lint_path = lint_instrument::resolve_lint_path(args.lint_path.clone())
        .context("resolve lint-path")?;

    lint_instrument::run_lint_instrument(LintInstrumentArgs {
        lint_path: resolved_lint_path,
        workspace_root: workspace_root.clone(),
        dry_run: false,
        allow_dirty: args.allow_dirty,
    })
    .context("instrument")?;

    // ── Step 3: build + run each bench ───────────────────────────────────────
    eprintln!(
        "measure: [3/5] bench run ({} target{})",
        args.benches.len(),
        if args.benches.len() == 1 { "" } else { "s" }
    );

    let dump_dir = workspace_root
        .join("target")
        .join("captrack-pgo")
        .join("dumps");
    std::fs::create_dir_all(&dump_dir)
        .with_context(|| format!("create dump dir {}", dump_dir.display()))?;

    let mut bench_reports: Vec<BenchReport> = Vec::new();

    for bench_name in &args.benches {
        let report = run_single_bench(
            bench_name,
            &workspace_root,
            args.cargo_target_dir.as_deref(),
            &dump_dir,
            args.bench_timeout,
        )
        .with_context(|| format!("bench {bench_name}"))?;
        bench_reports.push(report);
    }

    // ── Step 4: merge ─────────────────────────────────────────────────────────
    eprintln!("measure: [4/5] merge {} dumps", bench_reports.len());

    let profile_paths: Vec<PathBuf> = bench_reports
        .iter()
        .map(|r| r.profile_path.clone())
        .collect();

    // Filter to files that actually exist (bench may have produced nothing if
    // CAPTRACK_DUMP_DIR was not honoured).
    let existing: Vec<PathBuf> = profile_paths
        .iter()
        .filter(|p| p.is_file())
        .cloned()
        .collect();

    if existing.is_empty() {
        bail!(
            "no profile dump files found after bench runs; expected files in {}",
            dump_dir.display()
        );
    }

    let merge_report = merge::run_merge(MergeArgs {
        inputs: existing,
        output: args.out.clone(),
        reservoir_cap: args.reservoir_cap,
    })
    .context("merge")?;

    // ── Step 5: cleanup (explicit happy-path) ─────────────────────────────────
    eprintln!("measure: [5/5] uninstrument + unwire");
    guard.disarm(); // prevent double-cleanup in Drop
    cleanup_best_effort(&workspace_root);

    Ok(MeasureReport {
        benches_run: bench_reports,
        unique_sites: merge_report.unique_sites,
        merged_path: args.out.clone(),
    })
}

// ──────────────────────────────────────────────────────────────────────────────
// RAII cleanup guard
// ──────────────────────────────────────────────────────────────────────────────

/// Drop guard that runs uninstrument + unwire when dropped (i.e. on error or
/// panic) unless `disarm()` is called first.
///
/// This ensures the target workspace is always restored even when a step
/// between instrument and the explicit cleanup fails.
struct CleanupGuard {
    workspace_root: PathBuf,
    armed: bool,
}

impl CleanupGuard {
    fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            armed: true,
        }
    }

    /// Call before the explicit cleanup so Drop is a no-op.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if self.armed {
            eprintln!("measure: cleanup guard triggered (error/panic path)");
            cleanup_best_effort(&self.workspace_root);
        }
    }
}

/// Run uninstrument + unwire best-effort: log errors to stderr, never panic.
fn cleanup_best_effort(workspace_root: &Path) {
    // Uninstrument: revert instrument manifest if it exists.
    let manifest_path = default_instrument_manifest_path(workspace_root);
    if manifest_path.is_file() {
        match lint_apply::read_manifest(&manifest_path) {
            Ok(manifest) => {
                if matches!(manifest.operation, Operation::Instrument) {
                    if let Err(e) = lint_apply::undo_lint_apply(&manifest_path) {
                        eprintln!("cleanup: uninstrument error (ignored): {e}");
                    }
                }
            }
            Err(e) => {
                eprintln!("cleanup: read instrument manifest error (ignored): {e}");
            }
        }
    }

    // Unwire: revert wire manifest if it exists.
    let wire_manifest = wire::default_wire_manifest_path(workspace_root);
    if wire_manifest.is_file() {
        if let Err(e) = wire::run_unwire(UnwireArgs {
            workspace_root: workspace_root.to_path_buf(),
            manifest: None,
        }) {
            eprintln!("cleanup: unwire error (ignored): {e}");
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Per-bench orchestration
// ──────────────────────────────────────────────────────────────────────────────

/// Build and run a single bench target, collecting the captrack profile dump.
///
/// Returns a `BenchReport` pointing at the expected dump file.
fn run_single_bench(
    bench_name: &str,
    workspace_root: &Path,
    cargo_target_dir: Option<&Path>,
    dump_dir: &Path,
    timeout: Duration,
) -> Result<BenchReport> {
    // 1. Find which workspace member owns this bench target.
    let crate_name = find_bench_crate(bench_name, workspace_root)
        .with_context(|| format!("find crate for bench `{bench_name}`"))?;

    eprintln!("  bench: building {bench_name} (crate: {crate_name}) with --no-run");

    // 2. `cargo bench --no-run` to compile the bench binary.
    let mut build_cmd = std::process::Command::new("cargo");
    build_cmd.arg("bench");
    build_cmd.arg("--no-run");
    build_cmd.arg("-p");
    build_cmd.arg(&crate_name);
    build_cmd.arg("--bench");
    build_cmd.arg(bench_name);
    build_cmd.arg("--manifest-path");
    build_cmd.arg(workspace_root.join("Cargo.toml"));

    // Features: enable captrack telemetry so tracking is active.
    build_cmd.arg("--features");
    build_cmd.arg("captrack/telemetry");

    if let Some(target_dir) = cargo_target_dir {
        build_cmd.env("CARGO_TARGET_DIR", target_dir);
    }

    build_cmd.stdin(std::process::Stdio::null());

    let build_status = build_cmd
        .status()
        .context("spawn `cargo bench --no-run`")?;

    if !build_status.success() {
        bail!(
            "`cargo bench --no-run --bench {bench_name}` failed with {:?}",
            build_status.code()
        );
    }

    // 3. Discover the compiled bench binary.
    let target_root = if let Some(d) = cargo_target_dir {
        d.to_path_buf()
    } else {
        workspace_root.join("target")
    };

    let binary = find_bench_binary(bench_name, &target_root)
        .with_context(|| format!("find compiled binary for bench `{bench_name}`"))?;

    eprintln!("  bench: running {} (timeout: {}s)", binary.display(), timeout.as_secs());

    // 4. Run the bench binary with CAPTRACK_DUMP_DIR set.
    let dump_file = dump_dir.join(format!("profile-{bench_name}.json"));

    let mut run_cmd = std::process::Command::new(&binary);
    run_cmd.env("CAPTRACK_DUMP_DIR", dump_dir);
    run_cmd.stdin(std::process::Stdio::null());
    // Bench harnesses accept `--bench` to run in benchmark mode.
    run_cmd.arg("--bench");

    // Platform-compatible timeout: spawn + wait_timeout.
    let mut child = run_cmd
        .spawn()
        .with_context(|| format!("spawn bench binary {}", binary.display()))?;

    let exit_status = wait_with_timeout(&mut child, timeout)
        .with_context(|| format!("wait for bench {bench_name}"))?;

    let exit_code = exit_status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);

    if exit_code != 0 {
        eprintln!(
            "  warn: bench `{bench_name}` exited with code {exit_code} (profile may be partial)"
        );
    }

    Ok(BenchReport {
        name: bench_name.to_owned(),
        profile_path: dump_file,
        exit_code,
    })
}

// ──────────────────────────────────────────────────────────────────────────────
// Bench-crate auto-detect via `cargo metadata`
// ──────────────────────────────────────────────────────────────────────────────

/// Parse `cargo metadata` output to find which workspace member crate owns a
/// bench target with the given name.
///
/// Returns the crate's `name` field (used with `cargo bench -p <name>`).
pub fn find_bench_crate(bench_name: &str, workspace_root: &Path) -> Result<String> {
    // Invoke `cargo metadata --format-version 1 --no-deps`.
    let output = std::process::Command::new("cargo")
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps")
        .arg("--manifest-path")
        .arg(workspace_root.join("Cargo.toml"))
        .stdin(std::process::Stdio::null())
        .output()
        .context("spawn `cargo metadata`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "`cargo metadata` failed (exit {:?}):\n{stderr}",
            output.status.code()
        );
    }

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parse `cargo metadata` JSON")?;

    let packages = json["packages"]
        .as_array()
        .ok_or_else(|| anyhow!("`cargo metadata` output missing `packages` array"))?;

    for pkg in packages {
        let targets = match pkg["targets"].as_array() {
            Some(t) => t,
            None => continue,
        };
        for target in targets {
            // Check that this target has kind "bench".
            let kinds = match target["kind"].as_array() {
                Some(k) => k,
                None => continue,
            };
            let is_bench = kinds.iter().any(|k| k.as_str() == Some("bench"));
            if !is_bench {
                continue;
            }
            let target_name = match target["name"].as_str() {
                Some(n) => n,
                None => continue,
            };
            if target_name == bench_name {
                let crate_name = pkg["name"]
                    .as_str()
                    .ok_or_else(|| anyhow!("package missing `name` field"))?;
                return Ok(crate_name.to_owned());
            }
        }
    }

    bail!(
        "no workspace member has a bench target named `{bench_name}`; \
         run `cargo metadata --no-deps` to inspect available bench targets"
    )
}

// ──────────────────────────────────────────────────────────────────────────────
// Bench-binary discovery
// ──────────────────────────────────────────────────────────────────────────────

/// Find the compiled bench binary produced by `cargo bench --no-run --bench <name>`.
///
/// Cargo places bench binaries in `<target_root>/release/deps/` (or
/// `<target_root>/debug/deps/` for non-release).  The binary stem is
/// `<bench_name>-<hash>` (e.g. `tx_pipeline-a7f3c12d9e8b`).
///
/// We probe `release/deps/` first (typical for bench), then `debug/deps/`.
/// On Windows the extension is `.exe`; on other platforms we check the
/// executable bit via file metadata.
pub fn find_bench_binary(bench_name: &str, target_root: &Path) -> Result<PathBuf> {
    let candidates = [
        target_root.join("release").join("deps"),
        target_root.join("debug").join("deps"),
    ];

    for deps_dir in &candidates {
        if !deps_dir.is_dir() {
            continue;
        }
        let entries = std::fs::read_dir(deps_dir)
            .with_context(|| format!("read dir {}", deps_dir.display()))?;

        let mut matches: Vec<PathBuf> = Vec::new();

        for entry in entries {
            let entry = entry.with_context(|| format!("read entry in {}", deps_dir.display()))?;
            let path = entry.path();

            // Must be a file (not a dir or symlink to dir).
            if !path.is_file() {
                continue;
            }

            if !is_executable_binary(&path) {
                continue;
            }

            // Stem must be `<bench_name>-<hash>`.
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s,
                None => continue,
            };

            if is_bench_stem(stem, bench_name) {
                matches.push(path);
            }
        }

        if !matches.is_empty() {
            // If multiple hash variants exist (stale + current), pick the
            // most-recently-modified one.
            matches.sort_by_key(|p| {
                p.metadata()
                    .and_then(|m| m.modified())
                    .ok()
            });
            return Ok(matches.into_iter().last().unwrap());
        }
    }

    bail!(
        "could not find compiled bench binary for `{bench_name}` in {:?}; \
         ensure `cargo bench --no-run --bench {bench_name}` succeeded",
        candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()
    )
}

/// Returns `true` if `stem` matches the pattern `<bench_name>-<hex_hash>`.
///
/// Cargo uses a 16-character lowercase hex suffix (8 bytes / 64-bit hash),
/// separated by a single `-`.  We check that:
/// 1. `stem` starts with `<bench_name>-`.
/// 2. The suffix after the first `-<bench_name>` separator is non-empty and
///    consists solely of lowercase hex digits.
fn is_bench_stem(stem: &str, bench_name: &str) -> bool {
    let prefix = format!("{bench_name}-");
    if !stem.starts_with(&prefix) {
        return false;
    }
    let suffix = &stem[prefix.len()..];
    // Suffix must be non-empty and all lowercase hex.
    !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
}

/// Returns `true` if the path is an executable binary.
///
/// On Windows: file extension is `.exe`.
/// On Unix: file metadata says it's executable (any of user/group/other).
fn is_executable_binary(path: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("exe"))
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        use std::os::unix::fs::PermissionsExt as _;
        path.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Cross-platform timeout helper
// ──────────────────────────────────────────────────────────────────────────────

/// Wait for `child` to exit, killing it if `timeout` elapses.
///
/// Returns `Ok(Some(ExitStatus))` if the process exited within the timeout,
/// `Ok(None)` if the process was killed (timeout), or `Err` on OS error.
fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<Option<std::process::ExitStatus>> {
    use std::time::Instant;

    let deadline = Instant::now() + timeout;
    let poll_interval = Duration::from_millis(100);

    loop {
        match child.try_wait().context("try_wait on bench process")? {
            Some(status) => return Ok(Some(status)),
            None => {}
        }

        if Instant::now() >= deadline {
            eprintln!("  warn: bench timed out after {}s; killing", timeout.as_secs());
            let _ = child.kill();
            let _ = child.wait(); // reap zombie
            return Ok(None);
        }

        std::thread::sleep(poll_interval);
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Unit tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    // ── is_bench_stem ────────────────────────────────────────────────────────

    #[test]
    fn bench_stem_matches_valid_hash_suffix() {
        assert!(is_bench_stem("tx_pipeline-a7f3c12d9e8b0011", "tx_pipeline"));
        assert!(is_bench_stem("my_bench-0000000000000000", "my_bench"));
    }

    #[test]
    fn bench_stem_rejects_wrong_name() {
        assert!(!is_bench_stem("other_bench-a7f3c12d9e8b0011", "tx_pipeline"));
    }

    #[test]
    fn bench_stem_rejects_no_hash() {
        assert!(!is_bench_stem("tx_pipeline", "tx_pipeline"));
        assert!(!is_bench_stem("tx_pipeline-", "tx_pipeline"));
    }

    #[test]
    fn bench_stem_rejects_uppercase_hash() {
        // Cargo hashes are always lowercase; uppercase means it's not a bench binary.
        assert!(!is_bench_stem("tx_pipeline-A7F3C12D9E8B0011", "tx_pipeline"));
    }

    #[test]
    fn bench_stem_rejects_non_hex_suffix() {
        assert!(!is_bench_stem("tx_pipeline-gggggggggggggggg", "tx_pipeline"));
    }

    // ── find_bench_crate (cargo metadata parse) ──────────────────────────────

    /// Feed a synthetic cargo metadata JSON snippet and verify the parser
    /// finds the correct crate name for a bench target.
    #[test]
    fn find_bench_crate_parses_metadata_json() {
        // Write a minimal synthetic workspace with a bench target.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Write root Cargo.toml
        std::fs::write(
            root.join("Cargo.toml"),
            r#"
[workspace]
members = ["alpha"]
"#,
        )
        .unwrap();

        // Write member manifest
        let alpha_dir = root.join("alpha");
        std::fs::create_dir_all(alpha_dir.join("benches")).unwrap();
        std::fs::write(
            alpha_dir.join("Cargo.toml"),
            r#"
[package]
name = "alpha"
version = "0.1.0"
edition = "2021"

[[bench]]
name = "my_bench"
harness = false
"#,
        )
        .unwrap();

        // Write bench source so cargo metadata doesn't error on missing file.
        std::fs::write(
            alpha_dir.join("benches").join("my_bench.rs"),
            "fn main() {}\n",
        )
        .unwrap();

        // Also need a src/lib.rs or similar
        std::fs::create_dir_all(alpha_dir.join("src")).unwrap();
        std::fs::write(alpha_dir.join("src").join("lib.rs"), "").unwrap();

        // call cargo metadata on the synthetic workspace; this requires cargo to
        // be installed (true in CI and dev environments).
        let result = find_bench_crate("my_bench", root);
        match result {
            Ok(name) => assert_eq!(name, "alpha"),
            Err(e) => {
                // If cargo is not available or the test env lacks it, skip.
                let msg = e.to_string();
                if msg.contains("spawn") || msg.contains("No such file") || msg.contains("not found") {
                    eprintln!("skip: cargo not available: {e}");
                } else {
                    panic!("unexpected error: {e}");
                }
            }
        }
    }

    #[test]
    fn find_bench_crate_unknown_bench_name_errors() {
        // This test uses a minimal synthetic workspace where cargo metadata
        // succeeds but the bench name does not exist.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"alpha\"]\n",
        )
        .unwrap();
        let alpha = root.join("alpha");
        std::fs::create_dir_all(alpha.join("src")).unwrap();
        std::fs::write(
            alpha.join("Cargo.toml"),
            "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(alpha.join("src").join("lib.rs"), "").unwrap();

        let result = find_bench_crate("nonexistent_bench", root);
        match result {
            Err(e) => {
                let msg = e.to_string();
                if !msg.contains("spawn") && !msg.contains("No such file") && !msg.contains("not found") {
                    // cargo was available but bench was not found — correct
                    assert!(
                        msg.contains("nonexistent_bench"),
                        "error must mention the bench name; got: {msg}"
                    );
                }
                // else: cargo unavailable — acceptable in restricted envs
            }
            Ok(_) => panic!("expected error for missing bench target"),
        }
    }

    // ── MeasureArgs validation ────────────────────────────────────────────────

    #[test]
    fn measure_empty_benches_errors() {
        let tmp = tempfile::tempdir().unwrap();
        // Write a minimal Cargo.toml so workspace lookup succeeds.
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = []\n",
        )
        .unwrap();

        let result = run_measure(MeasureArgs {
            workspace: tmp.path().to_path_buf(),
            captrack_path: PathBuf::from("/nonexistent"),
            benches: vec![],
            lint_path: None,
            out: tmp.path().join("merged.json"),
            bench_timeout: Duration::from_secs(60),
            cargo_target_dir: None,
            reservoir_cap: 4096,
            allow_dirty: false,
        });

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("--bench"),
            "error must mention --bench; got: {msg}"
        );
    }

    #[test]
    fn measure_invalid_workspace_errors() {
        let tmp = tempfile::tempdir().unwrap();
        // No Cargo.toml — workspace lookup should fail.
        let bad_workspace = tmp.path().join("does_not_exist");
        std::fs::create_dir_all(&bad_workspace).unwrap();

        let result = run_measure(MeasureArgs {
            workspace: bad_workspace,
            captrack_path: PathBuf::from("/nonexistent"),
            benches: vec!["some_bench".to_owned()],
            lint_path: None,
            out: tmp.path().join("merged.json"),
            bench_timeout: Duration::from_secs(60),
            cargo_target_dir: None,
            reservoir_cap: 4096,
            allow_dirty: false,
        });

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("workspace") || msg.contains("Cargo.toml"),
            "error must mention workspace/Cargo.toml; got: {msg}"
        );
    }

    // ── CleanupGuard ─────────────────────────────────────────────────────────

    /// Verify `disarm()` prevents Drop from running cleanup.
    /// We do this by checking that `armed` is false after `disarm`.
    #[test]
    fn cleanup_guard_disarm_prevents_drop_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let mut guard = CleanupGuard::new(tmp.path().to_path_buf());
        assert!(guard.armed, "guard starts armed");
        guard.disarm();
        assert!(!guard.armed, "guard is disarmed after disarm()");
        // Drop happens here — cleanup_best_effort must NOT be called
        // (no panic, since the workspace has no manifests to revert).
    }

    /// Verify armed guard fires cleanup on drop.
    /// We create fake wire + instrument manifests and verify they are consumed.
    #[test]
    fn cleanup_guard_armed_runs_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Create minimal wire manifest so unwire doesn't bail.
        let captrack_dir = root.join("target").join("captrack-pgo");
        std::fs::create_dir_all(&captrack_dir).unwrap();

        // Wire manifest with zero files (valid v1, nothing to revert).
        let wire_manifest = serde_json::json!({
            "version": 1,
            "source": { "kind": "version", "version": "0.1" },
            "files": []
        });
        std::fs::write(
            captrack_dir.join("last-wire.json"),
            serde_json::to_string_pretty(&wire_manifest).unwrap(),
        )
        .unwrap();

        // Drop an armed guard — cleanup_best_effort runs.
        {
            let guard = CleanupGuard::new(root.to_path_buf());
            drop(guard); // armed → cleanup runs
        }

        // After cleanup, unwire deletes the wire manifest on success.
        assert!(
            !captrack_dir.join("last-wire.json").exists(),
            "wire manifest should be deleted by unwire"
        );
    }

    // ── find_bench_binary ────────────────────────────────────────────────────

    #[test]
    fn find_bench_binary_picks_most_recent() {
        let tmp = tempfile::tempdir().unwrap();
        let deps = tmp.path().join("release").join("deps");
        std::fs::create_dir_all(&deps).unwrap();

        // Create two fake bench binaries with the same name prefix.
        let b1 = deps.join(make_binary_name("my_bench", "aaaa1111bbbb2222"));
        let b2 = deps.join(make_binary_name("my_bench", "cccc3333dddd4444"));
        std::fs::write(&b1, b"fake binary 1").unwrap();
        std::thread::sleep(Duration::from_millis(10)); // ensure different mtime
        std::fs::write(&b2, b"fake binary 2").unwrap();

        // On Unix, mark executable.
        #[cfg(not(target_os = "windows"))]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for p in [&b1, &b2] {
                let mut perms = std::fs::metadata(p).unwrap().permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(p, perms).unwrap();
            }
        }

        let found = find_bench_binary("my_bench", tmp.path()).unwrap();
        // Should return b2 (most recently modified).
        assert_eq!(found, b2, "must return most recently modified binary");
    }

    #[test]
    fn find_bench_binary_returns_error_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("release").join("deps")).unwrap();

        let result = find_bench_binary("nonexistent_bench", tmp.path());
        assert!(result.is_err());
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Build an OS-appropriate binary file name for tests.
    fn make_binary_name(bench_name: &str, hash: &str) -> String {
        #[cfg(target_os = "windows")]
        {
            format!("{bench_name}-{hash}.exe")
        }
        #[cfg(not(target_os = "windows"))]
        {
            format!("{bench_name}-{hash}")
        }
    }

    /// Verify `is_executable_binary` matches OS conventions.
    #[test]
    fn executable_binary_detection() {
        let tmp = tempfile::tempdir().unwrap();

        #[cfg(target_os = "windows")]
        {
            let exe = tmp.path().join("test.exe");
            std::fs::write(&exe, b"").unwrap();
            assert!(is_executable_binary(&exe));

            let dll = tmp.path().join("test.dll");
            std::fs::write(&dll, b"").unwrap();
            assert!(!is_executable_binary(&dll));
        }

        #[cfg(not(target_os = "windows"))]
        {
            use std::os::unix::fs::PermissionsExt as _;

            let exe = tmp.path().join("test_bin");
            std::fs::write(&exe, b"").unwrap();
            let mut perms = std::fs::metadata(&exe).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&exe, perms).unwrap();
            assert!(is_executable_binary(&exe));

            let non_exe = tmp.path().join("test_data");
            std::fs::write(&non_exe, b"").unwrap();
            let mut perms2 = std::fs::metadata(&non_exe).unwrap().permissions();
            perms2.set_mode(0o644);
            std::fs::set_permissions(&non_exe, perms2).unwrap();
            assert!(!is_executable_binary(&non_exe));
        }
    }

    /// Verify `cleanup_best_effort` does not panic on a clean workspace.
    #[test]
    fn cleanup_best_effort_no_manifests_no_panic() {
        let tmp = tempfile::tempdir().unwrap();
        // No manifests exist → should be a no-op without panicking.
        cleanup_best_effort(tmp.path());
    }

    // ── is_bench_stem edge cases ──────────────────────────────────────────────

    #[test]
    fn bench_stem_handles_hyphenated_bench_name() {
        // Bench names with hyphens/underscores in them.
        assert!(is_bench_stem("my-bench-name-a1b2c3d4e5f60000", "my-bench-name"));
        assert!(!is_bench_stem("my-bench-name-a1b2c3d4e5f60000", "my_bench_name"));
    }

    #[test]
    fn bench_stem_rejects_partial_match() {
        // "tx_pipeline_extra-hash" must not match "tx_pipeline".
        assert!(!is_bench_stem("tx_pipeline_extra-a7f3c12d9e8b0011", "tx_pipeline"));
    }
}
