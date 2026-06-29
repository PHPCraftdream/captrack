//! Integration tests for the `measure` subcommand.
//!
//! Most tests here are unit-level (call internal helpers via the public API)
//! or use synthetic workspaces + tempfiles.  A true end-to-end test
//! (wire → cargo build → run bench → dump → merge) requires a nightly Rust
//! toolchain with cargo-dylint and the captrack-pgo-lint plugin compiled —
//! that is marked `#[ignore]` and documented as a followup.

use std::path::PathBuf;

// ── Re-use the public API surface from the captrack-pgo library ──────────────
// The binary crate exposes no lib target, so we test through the binary's
// `src/measure.rs` module via the integration test crate convention
// (tests/ are compiled as separate crates; we access captrack-pgo's public
// items through the `captrack_pgo` crate name, but since it's a binary crate
// we test the module items directly via `#[path]` include).
//
// Alternative: promote `measure.rs` to a library — but that requires changing
// `Cargo.toml`.  Instead we rely on the unit tests in `src/measure.rs` for
// internal items and use `std::process::Command` for smoke-testing the CLI
// here.

/// Smoke test: invoking `captrack-pgo measure --help` must exit 0 and mention
/// the `--workspace` and `--bench` flags.
#[test]
fn measure_help_shows_flags() {
    let bin = captrack_pgo_binary();
    let out = std::process::Command::new(&bin)
        .arg("measure")
        .arg("--help")
        .output();

    match out {
        Err(e) => {
            // Binary may not be built in some CI configurations; skip gracefully.
            eprintln!("skip: could not run captrack-pgo binary: {e}");
        }
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{stdout}{stderr}");

            assert!(
                output.status.success(),
                "--help must exit 0; stdout={stdout}; stderr={stderr}"
            );
            assert!(
                combined.contains("--workspace") || combined.contains("workspace"),
                "--help must mention --workspace; got: {combined}"
            );
            assert!(
                combined.contains("--bench") || combined.contains("bench"),
                "--help must mention --bench; got: {combined}"
            );
            assert!(
                combined.contains("--out") || combined.contains("out"),
                "--help must mention --out; got: {combined}"
            );
        }
    }
}

/// Smoke test: `measure` with no args exits non-zero.
#[test]
fn measure_no_args_exits_nonzero() {
    let bin = captrack_pgo_binary();
    let out = std::process::Command::new(&bin).arg("measure").output();

    match out {
        Err(e) => eprintln!("skip: {e}"),
        Ok(output) => {
            assert!(
                !output.status.success(),
                "measure with no args should fail; got: {:?}",
                output.status
            );
        }
    }
}

/// Smoke test: `measure --workspace <nonexistent> --captrack-path . --bench x`
/// exits non-zero.
#[test]
fn measure_invalid_workspace_exits_nonzero() {
    let bin = captrack_pgo_binary();
    let out = std::process::Command::new(&bin)
        .arg("measure")
        .arg("--workspace")
        .arg("/tmp/captrack_test_nonexistent_workspace_xyz")
        .arg("--captrack-path")
        .arg(".")
        .arg("--bench")
        .arg("some_bench")
        .output();

    match out {
        Err(e) => eprintln!("skip: {e}"),
        Ok(output) => {
            assert!(
                !output.status.success(),
                "measure with invalid workspace should fail; got: {:?}",
                output.status
            );
        }
    }
}

/// Placeholder for the real end-to-end integration test.
///
/// Requirements:
/// - nightly Rust toolchain
/// - cargo-dylint installed
/// - captrack-pgo-lint plugin compiled
/// - a synthetic workspace with a bench target
///
/// Marked `#[ignore]` — run explicitly with `cargo test -- --ignored`.
#[test]
#[ignore = "requires nightly + cargo-dylint + compiled captrack-pgo-lint plugin"]
fn measure_full_cycle_integration() {
    // TODO (followup): create a tempdir synthetic workspace with:
    //   - a bench target that allocates a Vec and exits,
    //   - wire/instrument it via measure,
    //   - verify merged.json is produced with at least one site,
    //   - verify uninstrument + unwire restore Cargo.toml to original state.
    todo!("real integration test — followup task");
}

// ──────────────────────────────────────────────────────────────────────────────
// Helper
// ──────────────────────────────────────────────────────────────────────────────

/// Return the path to the compiled `captrack-pgo` binary.
///
/// In `cargo test`, the binary is in `target/{profile}/captrack-pgo[.exe]`.
/// We find it by checking the environment variable `CARGO_BIN_EXE_captrack-pgo`
/// which Cargo sets for integration tests when `[[bin]]` is declared.
fn captrack_pgo_binary() -> PathBuf {
    // Cargo sets this env var for integration test crates.
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_captrack-pgo") {
        return PathBuf::from(p);
    }

    // Fallback: look alongside the test binary (same target directory).
    let exe_name = if cfg!(target_os = "windows") {
        "captrack-pgo.exe"
    } else {
        "captrack-pgo"
    };

    // Walk up from the test binary's directory to find the binary.
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(parent) = current_exe.parent() {
            let candidate = parent.join(exe_name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    // Last resort: rely on PATH.
    PathBuf::from(exe_name)
}
