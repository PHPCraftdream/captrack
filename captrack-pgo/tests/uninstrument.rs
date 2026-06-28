//! Integration tests for the `uninstrument` subcommand.
//!
//! All tests are structural — they write manifests and real files directly
//! without invoking `cargo dylint`, so they run unconditionally (no nightly
//! toolchain, no `cargo-dylint` install required).

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_captrack-pgo"))
}

/// Build a minimal `LintRunManifest` JSON for an `instrument` operation.
fn instrument_manifest_json(file: &std::path::Path, content_before: &str, sha_after: &str) -> String {
    let sha_before = sha256_hex(content_before.as_bytes());
    // Use serde_json::json! so strings are properly JSON-escaped.
    serde_json::json!({
        "version": 1,
        "operation": { "op": "instrument" },
        "files": [{
            "file": file.to_string_lossy().as_ref(),
            "sha256_before": sha_before,
            "content_before": content_before,
            "sha256_after": sha_after
        }]
    })
    .to_string()
}

/// Build a minimal `LintRunManifest` JSON for an `apply` operation.
fn apply_manifest_json() -> String {
    r#"{"version":1,"operation":{"op":"apply","profile_path":"/some/profile.json"},"files":[]}"#.to_string()
}

/// Hex SHA-256 (mirrors `lint_apply::hex_sha256`).
fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    // Use sha2 indirectly via a known-good computation for test data.
    // For simplicity, we call the captrack-pgo binary's own helper indirectly
    // by pre-computing in the test harness.  Since we control both the file
    // content and sha_after in the manifest, we compute it the same way.
    let digest = <sha2::Sha256 as sha2::Digest>::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 1 — happy path: restores files from an instrument manifest
// ──────────────────────────────────────────────────────────────────────────────

/// `uninstrument` with an `Instrument` manifest restores the original file
/// content and exits 0 with "reverted 1 file" in stdout.
#[test]
fn uninstrument_restores_files_from_instrument_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Create a fake workspace so --workspace resolution works.
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();

    // The "modified" file that uninstrument should revert.
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src_file = src_dir.join("lib.rs");
    let original_content = "fn original() {}";
    let modified_content = "fn modified_by_instrument() {}";
    std::fs::write(&src_file, modified_content).unwrap();

    let sha_after = sha256_hex(modified_content.as_bytes());

    // Write the instrument manifest at the conventional location.
    let manifest_dir = root.join("target").join("captrack-pgo");
    std::fs::create_dir_all(&manifest_dir).unwrap();
    let manifest_path = manifest_dir.join("last-instrument.json");
    let manifest_json = instrument_manifest_json(&src_file, original_content, &sha_after);
    std::fs::write(&manifest_path, &manifest_json).unwrap();

    let out = Command::new(bin())
        .args(["uninstrument", "--workspace"])
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "expected exit 0; stderr: {stderr}\nstdout: {stdout}"
    );

    // File must be restored to original content.
    let restored = std::fs::read_to_string(&src_file).unwrap();
    assert_eq!(
        restored, original_content,
        "file content must be restored to original"
    );

    // Stdout must mention "reverted 1 file".
    assert!(
        stdout.contains("reverted 1 file") || stdout.contains("reverted 1"),
        "expected 'reverted 1 file' in stdout; got:\n{stdout}"
    );

    // Manifest must still exist (left in place).
    assert!(manifest_path.exists(), "manifest must not be deleted");
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 2 — rejects apply manifests
// ──────────────────────────────────────────────────────────────────────────────

/// `uninstrument` exits non-zero when the manifest was produced by `apply`,
/// and the error message tells the user to use `captrack-pgo undo`.
#[test]
fn uninstrument_rejects_apply_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();

    // Write an apply manifest at the explicit path we'll pass to --manifest.
    let manifest_path = root.join("apply-manifest.json");
    std::fs::write(&manifest_path, apply_manifest_json()).unwrap();

    let out = Command::new(bin())
        .args(["uninstrument", "--manifest"])
        .arg(&manifest_path)
        .output()
        .expect("spawn captrack-pgo");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stderr}{stdout}");

    assert!(
        !out.status.success(),
        "expected non-zero exit for apply manifest; stdout: {stdout}\nstderr: {stderr}"
    );

    // Error must mention using `captrack-pgo undo`.
    assert!(
        combined.contains("undo"),
        "expected error mentioning 'undo'; got:\n{combined}"
    );
    // Error must distinguish `apply` from `instrument`.
    assert!(
        combined.contains("apply") || combined.contains("instrument"),
        "expected error distinguishing apply vs instrument; got:\n{combined}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 3 — helpful error when manifest file is missing
// ──────────────────────────────────────────────────────────────────────────────

/// `uninstrument --manifest <nonexistent>` exits non-zero with a helpful error
/// that mentions the missing path.
#[test]
fn uninstrument_missing_manifest_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does_not_exist.json");

    let out = Command::new(bin())
        .args(["uninstrument", "--manifest"])
        .arg(&missing)
        .output()
        .expect("spawn captrack-pgo");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stderr}{stdout}");

    assert!(
        !out.status.success(),
        "expected non-zero exit for missing manifest"
    );

    // Error must mention the path or the concept of "nothing to uninstrument".
    assert!(
        combined.contains("manifest") || combined.contains("uninstrument") || combined.contains("found"),
        "expected helpful error about missing manifest; got:\n{combined}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 4 — default path resolution uses last-instrument.json
// ──────────────────────────────────────────────────────────────────────────────

/// When `--manifest` is omitted, `uninstrument` uses the conventional
/// `target/captrack-pgo/last-instrument.json` path derived from the workspace
/// root.  Verify by placing the manifest there and running without `--manifest`.
#[test]
fn uninstrument_without_explicit_manifest_uses_default() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Minimal workspace.
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();

    // Source file to revert.
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src_file = src_dir.join("main.rs");
    let original = "fn main() { println!(\"original\"); }";
    let modified = "fn main() { println!(\"instrumented\"); }";
    std::fs::write(&src_file, modified).unwrap();

    let sha_after = sha256_hex(modified.as_bytes());

    // Place the manifest at the conventional default location.
    let manifest_dir = root.join("target").join("captrack-pgo");
    std::fs::create_dir_all(&manifest_dir).unwrap();
    let manifest_path = manifest_dir.join("last-instrument.json");
    let manifest_json = instrument_manifest_json(&src_file, original, &sha_after);
    std::fs::write(&manifest_path, &manifest_json).unwrap();

    // Run WITHOUT --manifest; only pass --workspace so the root is unambiguous.
    let out = Command::new(bin())
        .args(["uninstrument", "--workspace"])
        .arg(root)
        .output()
        .expect("spawn captrack-pgo");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "expected exit 0; stderr: {stderr}\nstdout: {stdout}"
    );

    // Confirm the default manifest was picked up (file is restored).
    let restored = std::fs::read_to_string(&src_file).unwrap();
    assert_eq!(
        restored, original,
        "file must be restored using the default manifest path"
    );
}
