//! End-to-end integration tests for `captrack-pgo auto`.
//!
//! These tests verify that the binary runs to completion and handles edge
//! cases correctly (empty dump, --apply with empty plan).  They do NOT
//! attempt to match exact AST line:col positions to keep the suite stable
//! across minor formatting changes.

use std::process::Command;

#[test]
fn auto_runs_to_completion_and_prints_something() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src").join("lib.rs"),
        "fn main() {\n    let _v: Vec<u32> = Vec::with_capacity(0);\n}\n",
    )
    .unwrap();

    // Empty dump → no matched sites, but binary should run cleanly.
    let dump = r#"{"version":1,"stats":[]}"#;
    let dump_path = root.join("dump.json");
    std::fs::write(&dump_path, dump).unwrap();

    let bin = std::path::PathBuf::from(env!("CARGO_BIN_EXE_captrack-pgo"));
    let out = Command::new(&bin)
        .args(["auto", "--workspace"])
        .arg(root)
        .arg("--captrack-dump")
        .arg(&dump_path)
        .output()
        .expect("spawn captrack-pgo");

    assert!(
        out.status.success(),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    // Must print something to stdout (report + dry-run notice).
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.is_empty(),
        "expected some stdout output, got nothing"
    );
}

#[test]
fn auto_with_apply_empty_plan_prints_nothing_to_apply() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    let src = root.join("src").join("lib.rs");
    let original = "fn main() {\n    let _v: Vec<u32> = Vec::with_capacity(0);\n}\n";
    std::fs::write(&src, original).unwrap();

    // Empty dump = empty plan → --apply should print "nothing to apply".
    let dump = r#"{"version":1,"stats":[]}"#;
    let dump_path = root.join("dump.json");
    std::fs::write(&dump_path, dump).unwrap();

    let bin = std::path::PathBuf::from(env!("CARGO_BIN_EXE_captrack-pgo"));
    let out = Command::new(&bin)
        .args(["auto", "--workspace"])
        .arg(root)
        .arg("--captrack-dump")
        .arg(&dump_path)
        .arg("--apply")
        .output()
        .expect("spawn captrack-pgo");

    assert!(
        out.status.success(),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("nothing to apply") || stdout.contains("no allocation sites"),
        "expected 'nothing to apply' or similar, got: {}",
        stdout
    );

    // Empty plan → no manifest written.
    let manifest_path = root
        .join("target")
        .join("captrack-pgo")
        .join("last-apply.json");
    assert!(
        !manifest_path.exists(),
        "empty plan must not write a manifest file"
    );
}

#[test]
fn auto_dry_run_prints_dry_run_notice() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src").join("lib.rs"),
        "fn main() {\n    let _v: Vec<u32> = Vec::with_capacity(0);\n}\n",
    )
    .unwrap();

    let dump = r#"{"version":1,"stats":[]}"#;
    let dump_path = root.join("dump.json");
    std::fs::write(&dump_path, dump).unwrap();

    let bin = std::path::PathBuf::from(env!("CARGO_BIN_EXE_captrack-pgo"));
    // Without --apply → dry-run notice.
    let out = Command::new(&bin)
        .args(["auto", "--workspace"])
        .arg(root)
        .arg("--captrack-dump")
        .arg(&dump_path)
        .output()
        .expect("spawn captrack-pgo");

    assert!(out.status.success());

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("dry-run") || stdout.contains("--apply"),
        "expected dry-run notice, got: {}",
        stdout
    );
}
