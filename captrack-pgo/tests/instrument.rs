//! Integration tests for the `instrument` subcommand.
//!
//! # How the tests work
//!
//! Each structural test spins up a temporary mini-workspace (a `[workspace]`
//! `Cargo.toml` plus a `src/lib.rs`) and invokes `captrack-pgo instrument`
//! as a subprocess.
//!
//! # Gate — `#[ignore]`
//!
//! The "live" test (that actually invokes `cargo dylint --fix`) is marked
//! `#[ignore]` because:
//!
//! 1. `cargo dylint` must build the `captrack-pgo-lint` cdylib against a
//!    pinned nightly toolchain, which takes several minutes on first run.
//! 2. The nightly toolchain may not be installed in CI or on all developer
//!    machines.
//! 3. The workspace being instrumented must depend on `captrack` for the
//!    rewritten code to compile.
//!
//! To run the live test locally:
//!   ```text
//!   rustup toolchain install nightly-2026-04-16
//!   cargo test --test instrument -- --ignored
//!   ```
//!
//! The "structural" tests (pre-flight checks, missing-path errors, dep
//! warnings) do NOT require nightly or dylint and run unconditionally.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_captrack-pgo"))
}

/// Minimal temporary workspace — a `[workspace]` root plus one member crate
/// `mylib` with the given `src/lib.rs` content.
///
/// Returns `(tmp_dir, lib_rs_path)`.
fn make_workspace(src_content: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"mylib\"]\n",
    )
    .unwrap();

    let pkg = root.join("mylib");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::write(
        pkg.join("Cargo.toml"),
        "[package]\nname = \"mylib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let lib_rs = pkg.join("src").join("lib.rs");
    std::fs::write(&lib_rs, src_content).unwrap();

    (tmp, lib_rs)
}

/// Same as `make_workspace` but the member's `Cargo.toml` includes
/// `captrack` with the `telemetry` feature, so the instrumented code
/// would compile.
fn make_workspace_with_captrack(src_content: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"mylib\"]\n",
    )
    .unwrap();

    let pkg = root.join("mylib");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    // Member Cargo.toml with captrack + telemetry.
    let captrack_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let captrack_path_str = captrack_path.to_string_lossy().replace('\\', "/");
    std::fs::write(
        pkg.join("Cargo.toml"),
        format!(
            "[package]\nname = \"mylib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ncaptrack = {{ path = \"{captrack_path_str}\", features = [\"telemetry\"] }}\n"
        ),
    )
    .unwrap();
    let lib_rs = pkg.join("src").join("lib.rs");
    std::fs::write(&lib_rs, src_content).unwrap();

    (tmp, lib_rs)
}

fn lint_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("captrack-pgo-lint")
}

// ──────────────────────────────────────────────────────────────────────────────
// Structural tests — run unconditionally, do NOT invoke cargo dylint
// ──────────────────────────────────────────────────────────────────────────────

/// `--dry-run` must NOT modify any source file.
///
/// We supply a valid lint-path so pre-flight passes; in dry-run the actual
/// `cargo dylint` invocation runs WITHOUT `--fix`, so it hits cargo's
/// "missing toolchain" error before touching files.  We only care that files
/// are unchanged — the exit code may be non-zero (dylint needs nightly) which
/// is acceptable for a structural test.
#[test]
fn dry_run_does_not_modify_files() {
    let src = "pub fn f() { let _v = Vec::new(); }\n";
    let (tmp, lib_rs) = make_workspace(src);
    let root = tmp.path();

    // Run with --dry-run.  We don't assert exit 0 here because cargo dylint
    // may fail if nightly isn't installed — that's fine; what we check is that
    // the source file is untouched.
    let _out = Command::new(bin())
        .args(["instrument", "--dry-run"])
        .arg("--lint-path")
        .arg(lint_path())
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    // Source must be unchanged regardless of exit code.
    let after = std::fs::read_to_string(&lib_rs).unwrap();
    assert_eq!(after, src, "dry-run must not modify source files");

    // No manifest must be written.
    let manifest = root
        .join("target")
        .join("captrack-pgo")
        .join("last-instrument.json");
    assert!(!manifest.exists(), "dry-run must not write a manifest");
}

/// Invalid `--lint-path` (directory does not contain Cargo.toml) must cause a
/// non-zero exit with a clear error message.
#[test]
fn missing_lint_path_exits_with_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();

    let out = Command::new(bin())
        .args(["instrument", "--lint-path", "/nonexistent/lint/path"])
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    assert!(
        !out.status.success(),
        "expected non-zero exit for bad lint-path"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.contains("Cargo.toml") || combined.contains("lint-path") || combined.contains("nonexistent"),
        "expected error about lint-path; got:\n{combined}"
    );
}

/// Workspace without a `Cargo.toml` must cause a non-zero exit.
#[test]
fn missing_workspace_exits_with_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // No Cargo.toml — `find_workspace_root` will fail.

    let out = Command::new(bin())
        .args(["instrument"])
        .arg("--lint-path")
        .arg(lint_path())
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    assert!(
        !out.status.success(),
        "expected non-zero exit when workspace has no Cargo.toml"
    );
}

/// When the workspace has no `captrack` dependency the subcommand must still
/// exit 0 (the warning is non-fatal) AND the warning text must appear in
/// stderr — but because pre-flight still runs `cargo dylint` (or in dry-run
/// mode exits cleanly), we test in dry-run so the check doesn't block on
/// cargo-dylint availability.
///
/// Strategy: run in `--dry-run` which executes `cargo dylint` without `--fix`.
/// `cargo dylint` will fail if nightly is missing, so exit code is unreliable.
/// Instead we only check that the warning is emitted before the process spawns
/// any child, which happens before the actual dylint invocation.
///
/// Simpler: call the binary, collect stderr, and assert the warning substring
/// is present REGARDLESS of exit code.  The warning is emitted synchronously
/// before `cargo dylint` is spawned.
#[test]
fn warns_when_captrack_not_a_dep() {
    let src = "pub fn f() { let _v = Vec::new(); }\n";
    let (tmp, _lib_rs) = make_workspace(src); // no captrack dep
    let root = tmp.path();

    let out = Command::new(bin())
        .args(["instrument", "--dry-run"])
        .arg("--lint-path")
        .arg(lint_path())
        .arg("--workspace")
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("captrack") && stderr.contains("warning"),
        "expected captrack-dep warning in stderr; got:\n{stderr}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Live test — requires nightly + cargo-dylint; #[ignore] by default.
//
// Run with: cargo test --test instrument -- --ignored
// ──────────────────────────────────────────────────────────────────────────────

/// `instrument` rewrites bare `Vec::new()` to `TrackedVec` form and writes a
/// manifest.
///
/// # Caveats
/// - Requires `cargo-dylint` (`cargo install cargo-dylint dylint-link`).
/// - Requires the pinned nightly toolchain (`nightly-2026-04-16`).
/// - The workspace must depend on `captrack` for the rewritten code to compile
///   (this test sets up such a workspace via `make_workspace_with_captrack`).
/// - On first run, `cargo dylint` downloads the nightly toolchain and compiles
///   the cdylib (~5–10 min); subsequent runs use the cache.
#[test]
#[ignore = "requires nightly toolchain + cargo-dylint (cargo test --test instrument -- --ignored)"]
fn live_instrument_rewrites_vec_new_and_writes_manifest() {
    let src = "pub fn f() { let _v: Vec<u8> = Vec::new(); }\n";
    let (tmp, lib_rs) = make_workspace_with_captrack(src);
    let root = tmp.path();

    let out = Command::new(bin())
        .args(["instrument"])
        .arg("--lint-path")
        .arg(lint_path())
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

    // lib.rs must now contain the TrackedVec instrumentation.
    let after = std::fs::read_to_string(&lib_rs).unwrap();
    assert!(
        after.contains("TrackedVec") || after.contains("with_capacity_named"),
        "expected TrackedVec or with_capacity_named in instrumented source; got:\n{after}"
    );

    // Manifest must exist.
    let manifest = root
        .join("target")
        .join("captrack-pgo")
        .join("last-instrument.json");
    assert!(manifest.exists(), "manifest must be written");

    let manifest_json =
        serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&manifest).unwrap())
            .unwrap();
    assert_eq!(manifest_json["version"], 1);
    assert_eq!(manifest_json["operation"]["op"], "instrument");
    assert!(
        !manifest_json["files"].as_array().unwrap().is_empty(),
        "at least one file entry expected"
    );
}
