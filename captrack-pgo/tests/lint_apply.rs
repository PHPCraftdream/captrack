//! Integration tests for the `apply` subcommand (Dylint-based rewrite).
//!
//! # How the tests work
//!
//! Each test spins up a temporary mini-workspace (a `[workspace]` `Cargo.toml`
//! plus a `src/lib.rs`) and invokes `captrack-pgo apply` as a subprocess.
//!
//! # Gate — `#[ignore]`
//!
//! The "live" tests (those that actually invoke `cargo dylint --fix`) are
//! marked `#[ignore]` because:
//!
//! 1. `cargo dylint` must build the `captrack-pgo-lint` cdylib against a
//!    pinned nightly toolchain (`nightly-2026-04-16`), which takes several
//!    minutes on first run (subsequent runs use the incremental cache).
//! 2. The nightly toolchain may not be installed in CI or on all developer
//!    machines.
//! 3. They require `LINT_PATH` to be resolvable (either the sibling directory
//!    or explicitly set via `CAPTRACK_LINT_PATH`).
//!
//! To run the live tests locally:
//!   ```text
//!   rustup toolchain install nightly-2026-04-16
//!   cargo test --test lint_apply -- --ignored
//!   ```
//!
//! The "structural" tests (pre-flight checks, dry-run output, manifest
//! format) do NOT require nightly or dylint and run unconditionally.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_captrack-pgo"))
}

/// Minimal temp workspace with a single `src/lib.rs`.
///
/// Returns `(tmp_dir, src_lib_rs_path, profile_json_path)`.
fn make_workspace(src_content: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    // Minimal workspace Cargo.toml — apply does not require workspace
    // members; it needs [workspace] and Cargo.toml to exist.
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"mylib\"]\n",
    )
    .unwrap();

    // Member crate.
    let pkg = root.join("mylib");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::write(
        pkg.join("Cargo.toml"),
        "[package]\nname = \"mylib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let lib_rs = pkg.join("src").join("lib.rs");
    std::fs::write(&lib_rs, src_content).unwrap();

    // Profile JSON — one entry pointing at lib_rs line 1, col 14.
    // (col 14 = start of `Vec::new()` in "pub fn f() { Vec::new() }")
    let abs = lib_rs.canonicalize().unwrap_or_else(|_| lib_rs.clone());
    let profile = serde_json::json!({
        "version": 1,
        "stats": [{
            "name": "test_vec",
            "file": abs.to_string_lossy(),
            "line": 1,
            "column": 14,
            "creation_count": 100,
            "samples": vec![512usize; 100]
        }]
    });
    let profile_path = root.join("profile.json");
    std::fs::write(&profile_path, serde_json::to_string_pretty(&profile).unwrap()).unwrap();

    (tmp, lib_rs, profile_path)
}

// ──────────────────────────────────────────────────────────────────────────────
// Structural tests — run unconditionally, do NOT invoke cargo dylint
// ──────────────────────────────────────────────────────────────────────────────

/// `--dry-run` must succeed, print the "dry-run" notice, NOT modify any file,
/// NOT write a manifest.  Supply a real lint-path so pre-flight passes,
/// then dry-run will print without actually running cargo-dylint.
#[test]
fn dry_run_prints_notice_and_does_not_modify_files() {
    let src = "pub fn f() { let _v = Vec::new(); }\n";
    let (tmp, lib_rs, profile_path) = make_workspace(src);
    let root = tmp.path();

    // Locate captrack-pgo-lint relative to the manifest dir of this crate.
    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--dry-run", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "expected exit 0; stderr: {stderr}\nstdout: {stdout}"
    );

    // Must mention "dry-run".
    assert!(
        stdout.contains("dry-run") || stdout.contains("dry run"),
        "expected 'dry-run' in stdout; got:\n{stdout}"
    );

    // Files must be unchanged.
    let after = std::fs::read_to_string(&lib_rs).unwrap();
    assert_eq!(after, src, "dry-run must not modify source files");

    // No manifest written.
    let manifest = root
        .join("target")
        .join("captrack-pgo")
        .join("last-apply.json");
    assert!(!manifest.exists(), "dry-run must not write a manifest");
}

/// Missing profile file → non-zero exit with a clear error message.
#[test]
fn missing_profile_exits_with_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--profile", "/nonexistent/profile.json"])
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    assert!(
        !out.status.success(),
        "expected non-zero exit for missing profile"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.contains("does not exist") || combined.contains("profile"),
        "expected error about profile; got:\n{combined}"
    );
}

/// Invalid `--lint-path` (does not contain Cargo.toml) → non-zero exit.
#[test]
fn invalid_lint_path_exits_with_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();

    // Create a tiny profile.
    let profile_path = root.join("p.json");
    std::fs::write(&profile_path, r#"{"version":1,"stats":[]}"#).unwrap();

    let out = Command::new(bin())
        .args(["apply", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg("/nonexistent/lint/path")
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    assert!(
        !out.status.success(),
        "expected non-zero exit for bad lint-path"
    );
}

/// Missing workspace Cargo.toml → non-zero exit.
#[test]
fn missing_workspace_exits_with_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // No Cargo.toml written.

    let profile_path = root.join("p.json");
    std::fs::write(&profile_path, r#"{"version":1,"stats":[]}"#).unwrap();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    assert!(
        !out.status.success(),
        "expected non-zero exit for missing workspace"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Hasher-choice structural tests — run unconditionally
// ──────────────────────────────────────────────────────────────────────────────

/// `--dry-run` with `--hasher fx` must mention the hasher in the dry-run output.
///
/// This is a structural test: it checks that `LintApplyArgs.hasher = HasherChoice::Fx`
/// correctly propagates to the dry-run printout (which includes the env var).
/// We don't need to actually run `cargo dylint` — the dry-run path prints the
/// would-be invocation, including `CAPTRACK_PGO_HASHER=fx`.
#[test]
fn hasher_choice_passes_env_var_to_dylint() {
    let src = "pub fn f() { let _v = Vec::new(); }\n";
    let (tmp, _lib_rs, profile_path) = make_workspace(src);
    let root = tmp.path();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--dry-run", "--hasher", "fx", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "expected exit 0; stderr: {stderr}\nstdout: {stdout}"
    );

    // The dry-run output must mention the hasher env var.
    assert!(
        stdout.contains("CAPTRACK_PGO_HASHER=fx"),
        "expected 'CAPTRACK_PGO_HASHER=fx' in dry-run stdout; got:\n{stdout}"
    );
}

/// `--hasher none` (the default) must NOT mention `CAPTRACK_PGO_HASHER` in the
/// dry-run output (the env var is removed when hasher=none).
#[test]
fn hasher_none_does_not_set_env_var() {
    let src = "pub fn f() { let _v = Vec::new(); }\n";
    let (tmp, _lib_rs, profile_path) = make_workspace(src);
    let root = tmp.path();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--dry-run", "--hasher", "none", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "expected exit 0; stderr: {stderr}\nstdout: {stdout}"
    );

    // Must NOT mention CAPTRACK_PGO_HASHER.
    assert!(
        !stdout.contains("CAPTRACK_PGO_HASHER"),
        "expected no CAPTRACK_PGO_HASHER in dry-run stdout when hasher=none; got:\n{stdout}"
    );
}

/// `--hasher` with an invalid value must exit non-zero.
#[test]
fn invalid_hasher_value_is_rejected_by_clap() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    let profile_path = root.join("p.json");
    std::fs::write(&profile_path, r#"{"version":1,"stats":[]}"#).unwrap();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--hasher", "xxhash", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    assert!(
        !out.status.success(),
        "expected non-zero exit for unknown hasher value"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("xxhash") || stderr.contains("unknown") || stderr.contains("invalid"),
        "expected error about invalid hasher; got:\n{stderr}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Capacity policy flag tests (M11) — run unconditionally (dry-run only)
// ──────────────────────────────────────────────────────────────────────────────

/// `--cap-from max` in dry-run must mention `CAPTRACK_PGO_CAP_FROM=max` in the
/// output.
#[test]
fn cap_policy_flags_pass_env_vars() {
    let src = "pub fn f() { let _v = Vec::new(); }\n";
    let (tmp, _lib_rs, profile_path) = make_workspace(src);
    let root = tmp.path();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args([
            "apply", "--dry-run",
            "--cap-from", "max",
            "--cap-mul", "1.5",
            "--cap-round", "exact",
            "--profile",
        ])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "expected exit 0; stderr: {stderr}\nstdout: {stdout}"
    );

    assert!(
        stdout.contains("CAPTRACK_PGO_CAP_FROM=max"),
        "expected CAPTRACK_PGO_CAP_FROM=max in dry-run stdout; got:\n{stdout}"
    );
    assert!(
        stdout.contains("CAPTRACK_PGO_CAP_MUL=1.5"),
        "expected CAPTRACK_PGO_CAP_MUL=1.5 in dry-run stdout; got:\n{stdout}"
    );
    assert!(
        stdout.contains("CAPTRACK_PGO_CAP_ROUND=exact"),
        "expected CAPTRACK_PGO_CAP_ROUND=exact in dry-run stdout; got:\n{stdout}"
    );
}

/// Default `--cap-from p95 --cap-mul 1.0 --cap-round pow2` must NOT set
/// `CAPTRACK_PGO_CAP_FROM`, `CAPTRACK_PGO_CAP_MUL`, or `CAPTRACK_PGO_CAP_ROUND`
/// env vars (they are omitted when equal to the plugin's own default to keep
/// the environment minimal).
#[test]
fn cap_policy_defaults_omit_env_vars() {
    let src = "pub fn f() { let _v = Vec::new(); }\n";
    let (tmp, _lib_rs, profile_path) = make_workspace(src);
    let root = tmp.path();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args([
            "apply", "--dry-run",
            "--cap-from", "p95",
            "--cap-mul", "1.0",
            "--cap-round", "pow2",
            "--profile",
        ])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "expected exit 0; stderr: {stderr}\nstdout: {stdout}"
    );

    assert!(
        !stdout.contains("CAPTRACK_PGO_CAP_FROM"),
        "expected no CAPTRACK_PGO_CAP_FROM for default p95; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("CAPTRACK_PGO_CAP_MUL"),
        "expected no CAPTRACK_PGO_CAP_MUL for default 1.0; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("CAPTRACK_PGO_CAP_ROUND"),
        "expected no CAPTRACK_PGO_CAP_ROUND for default pow2; got:\n{stdout}"
    );
}

/// `--cap-mul 0.0` must exit non-zero with a clear error.
#[test]
fn cap_mul_zero_is_rejected() {
    let src = "pub fn f() { let _v = Vec::new(); }\n";
    let (tmp, _lib_rs, profile_path) = make_workspace(src);
    let root = tmp.path();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--dry-run", "--cap-mul", "0.0", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    assert!(
        !out.status.success(),
        "expected non-zero exit for --cap-mul 0.0"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.contains("cap-mul") || combined.contains("0"),
        "expected error about cap-mul; got:\n{combined}"
    );
}

/// `--cap-mul -1.0` (negative) must exit non-zero.
#[test]
fn cap_mul_negative_is_rejected() {
    let src = "pub fn f() { let _v = Vec::new(); }\n";
    let (tmp, _lib_rs, profile_path) = make_workspace(src);
    let root = tmp.path();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--dry-run", "--cap-mul", "-1.0", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    assert!(
        !out.status.success(),
        "expected non-zero exit for --cap-mul -1.0"
    );
}

/// `--cap-from` with an invalid value must exit non-zero.
#[test]
fn invalid_cap_from_is_rejected_by_clap() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    let profile_path = root.join("p.json");
    std::fs::write(&profile_path, r#"{"version":1,"stats":[]}"#).unwrap();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--cap-from", "p50", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    assert!(
        !out.status.success(),
        "expected non-zero exit for unknown --cap-from value"
    );
}

/// `--cap-round` with an invalid value must exit non-zero.
#[test]
fn invalid_cap_round_is_rejected_by_clap() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    let profile_path = root.join("p.json");
    std::fs::write(&profile_path, r#"{"version":1,"stats":[]}"#).unwrap();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--cap-round", "round16", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    assert!(
        !out.status.success(),
        "expected non-zero exit for unknown --cap-round value"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Staleness-guard structural tests — run unconditionally (dry-run only)
//
// `apply --dry-run` runs the same pre-flight sequence as a real `apply`
// (including the staleness check) before printing its "would run" notice and
// returning early, so these tests exercise the guard through the CLI without
// requiring nightly or cargo-dylint.
// ──────────────────────────────────────────────────────────────────────────────

/// Positive path: no staleness manifest exists at all (no prior `instrument`
/// run) → the guard is a no-op and `--dry-run` succeeds normally.
#[test]
fn apply_dry_run_succeeds_when_no_staleness_manifest() {
    let src = "pub fn f() { let _v = Vec::new(); }\n";
    let (tmp, _lib_rs, profile_path) = make_workspace(src);
    let root = tmp.path();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--dry-run", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0 with no staleness manifest; stderr: {stderr}\nstdout: {stdout}"
    );
}

/// Positive path: a staleness manifest exists and every recorded file still
/// matches its recorded hash → `apply` proceeds (does not bail on staleness).
#[test]
fn apply_dry_run_succeeds_when_files_unchanged_since_instrument() {
    let src = "pub fn f() { let _v = Vec::new(); }\n";
    let (tmp, lib_rs, profile_path) = make_workspace(src);
    let root = tmp.path();

    write_fake_staleness_manifest(root, &[&lib_rs]);

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--dry-run", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0 when files unchanged since instrument; stderr: {stderr}\nstdout: {stdout}"
    );
}

/// Negative path: a staleness manifest exists but the tracked file was
/// modified afterwards → `apply` bails before reaching the dry-run notice,
/// with an error naming the changed file and hinting at `--force`.
#[test]
fn apply_bails_when_file_changed_since_instrument() {
    let src = "pub fn f() { let _v = Vec::new(); }\n";
    let (tmp, lib_rs, profile_path) = make_workspace(src);
    let root = tmp.path();

    write_fake_staleness_manifest(root, &[&lib_rs]);

    // Modify the tracked file AFTER the staleness snapshot was recorded.
    std::fs::write(&lib_rs, "pub fn f() { let _v = Vec::new(); } // changed\n").unwrap();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--dry-run", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    assert!(
        !out.status.success(),
        "expected non-zero exit when a tracked file changed since instrument"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.contains("lib.rs"),
        "expected the changed file name in the error; got:\n{combined}"
    );
    assert!(
        combined.contains("--force"),
        "expected a --force hint in the error; got:\n{combined}"
    );
    // Must not have reached the dry-run notice.
    assert!(
        !stdout.contains("dry-run — would run"),
        "guard must bail before the dry-run notice; got:\n{stdout}"
    );
}

/// `--force` path: same modified-file scenario as above, but with `--force`
/// passed → the staleness check is bypassed and `apply` proceeds normally.
#[test]
fn apply_force_bypasses_staleness_check() {
    let src = "pub fn f() { let _v = Vec::new(); }\n";
    let (tmp, lib_rs, profile_path) = make_workspace(src);
    let root = tmp.path();

    write_fake_staleness_manifest(root, &[&lib_rs]);
    std::fs::write(&lib_rs, "pub fn f() { let _v = Vec::new(); } // changed\n").unwrap();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--dry-run", "--force", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected exit 0 with --force despite changed file; stderr: {stderr}\nstdout: {stdout}"
    );
}

/// Write a fake `last-instrument-hashes.json` staleness manifest recording
/// the CURRENT sha256 of each given file, mirroring the format written by
/// `captrack_pgo::staleness::write_staleness_snapshot`.
fn write_fake_staleness_manifest(workspace_root: &Path, files: &[&Path]) {
    use sha2::{Digest, Sha256};

    let dir = workspace_root.join("target").join("captrack-pgo");
    std::fs::create_dir_all(&dir).unwrap();

    let entries: Vec<serde_json::Value> = files
        .iter()
        .map(|f| {
            let content = std::fs::read_to_string(f).unwrap();
            let digest = Sha256::digest(content.as_bytes());
            let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            serde_json::json!({
                "path": f.canonicalize().unwrap_or_else(|_| f.to_path_buf()),
                "sha256": hex,
            })
        })
        .collect();

    let manifest = serde_json::json!({
        "version": 1,
        "timestamp": 0,
        "files": entries,
    });

    std::fs::write(
        dir.join("last-instrument-hashes.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

// ──────────────────────────────────────────────────────────────────────────────
// Live tests — require nightly + cargo-dylint; #[ignore] by default.
//
// Run with: cargo test --test lint_apply -- --ignored
// ──────────────────────────────────────────────────────────────────────────────

/// `apply` applies `--fix` suggestions to a minimal workspace, modifies
/// the source, and writes a manifest.
///
/// # Caveats
/// - Requires `cargo-dylint` (cargo install cargo-dylint dylint-link).
/// - Requires the pinned nightly toolchain (nightly-2026-04-16).
/// - Requires the `captrack-pgo-lint` cdylib to be buildable.
/// - On first run, `cargo dylint` downloads the nightly toolchain and compiles
///   the cdylib (~5–10 min); subsequent runs use the cache.
#[test]
#[ignore = "requires nightly toolchain + cargo-dylint (cargo test --test lint_apply -- --ignored)"]
fn live_apply_rewrites_vec_new_and_writes_manifest() {
    let src = "pub fn f() { let _v: Vec<u8> = Vec::new(); }\n";
    let (tmp, lib_rs, profile_path) = make_workspace(src);
    let root = tmp.path();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .arg("--allow-dirty")
        .output()
        .expect("spawn captrack-pgo");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "expected exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );

    // lib.rs must now contain with_capacity.
    let after = std::fs::read_to_string(&lib_rs).unwrap();
    assert!(
        after.contains("with_capacity"),
        "expected 'with_capacity' in patched source; got:\n{after}"
    );

    // Manifest must exist.
    let manifest = root
        .join("target")
        .join("captrack-pgo")
        .join("last-apply.json");
    assert!(manifest.exists(), "manifest must be written");

    let manifest_json =
        serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&manifest).unwrap())
            .unwrap();
    assert_eq!(manifest_json["version"], 1);
    assert!(
        !manifest_json["files"].as_array().unwrap().is_empty(),
        "at least one file entry expected"
    );
}

/// `--dry-run` in live mode: does NOT modify file, does NOT write manifest,
/// prints "would inspect".
#[test]
#[ignore = "requires nightly toolchain + cargo-dylint (cargo test --test lint_apply -- --ignored)"]
fn live_dry_run_does_not_modify_and_no_manifest() {
    let src = "pub fn f() { let _v: Vec<u8> = Vec::new(); }\n";
    let (tmp, lib_rs, profile_path) = make_workspace(src);
    let root = tmp.path();

    let lint_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("captrack-pgo-lint");

    let out = Command::new(bin())
        .args(["apply", "--dry-run", "--profile"])
        .arg(&profile_path)
        .arg("--lint-path")
        .arg(&lint_path)
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "expected exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Mention "dry-run".
    assert!(
        stdout.contains("dry-run") || stdout.contains("dry run") || stdout.contains("would"),
        "expected dry-run notice; got:\n{stdout}"
    );

    // File unchanged.
    let after = std::fs::read_to_string(&lib_rs).unwrap();
    assert_eq!(after, src, "dry-run must not modify file");

    // No manifest.
    let manifest = root
        .join("target")
        .join("captrack-pgo")
        .join("last-apply.json");
    assert!(!manifest.exists(), "dry-run must not write manifest");
}
