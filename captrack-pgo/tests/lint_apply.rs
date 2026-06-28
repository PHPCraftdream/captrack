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
        .join("last-lint-apply.json");
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
        .join("last-lint-apply.json");
    assert!(manifest.exists(), "manifest must be written");

    let manifest_json =
        serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&manifest).unwrap())
            .unwrap();
    assert_eq!(manifest_json["version"], 1);
    assert!(
        manifest_json["files"].as_array().unwrap().len() >= 1,
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
        .join("last-lint-apply.json");
    assert!(!manifest.exists(), "dry-run must not write manifest");
}
