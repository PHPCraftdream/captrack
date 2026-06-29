// Auto-dump capacity statistics on process exit.
//
// Compiled only with the `telemetry` feature.  Registers a `#[ctor::dtor]`
// destructor that runs after `main` returns: it computes a deterministic
// per-binary output path and writes the same JSON as `dump_capacity_stats`.
//
// This removes the need for callers to manually call `dump_capacity_stats(...)`
// at the end of every benchmark `main`.  Criterion-driven benches never
// expose their `main` directly, so without an exit hook the registry would
// be silently dropped on process termination.
//
// # Output path
//
// `<dir>/profile-<binary_stem>.json` where:
//
// * `<dir>` is read from the `CAPTRACK_DUMP_DIR` environment variable, falling
//   back to `target/captrack-pgo` if unset.
// * `<binary_stem>` is the file stem of `std::env::current_exe()`, falling back
//   to `"unknown"` if the platform cannot report it.
//
// The directory is created on demand (`dump_capacity_stats` does this already).
//
// # Opt-out
//
// Set `CAPTRACK_AUTODUMP=0` (or `off` / `false`) before the binary runs to
// suppress the on-exit write entirely.  Manual `dump_capacity_stats(path)`
// calls always work regardless.

use std::path::PathBuf;

/// Compute the auto-dump output path: `<CAPTRACK_DUMP_DIR>/profile-<binary_stem>.json`.
fn default_dump_path() -> PathBuf {
    let dir = std::env::var_os("CAPTRACK_DUMP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/captrack-pgo"));
    let stem = std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "unknown".to_string());
    dir.join(format!("profile-{stem}.json"))
}

/// Returns true unless the user explicitly disabled auto-dump via
/// `CAPTRACK_AUTODUMP=0|off|false|no`.
fn autodump_enabled() -> bool {
    match std::env::var("CAPTRACK_AUTODUMP") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "false" | "no"
        ),
        Err(_) => true,
    }
}

/// Destructor wired via `ctor::dtor` — runs after `main` returns (or after
/// the last user thread terminates).  Errors are deliberately swallowed:
/// failing to dump must not crash the host process during shutdown.
#[ctor::dtor]
fn captrack_autodump_on_exit() {
    if !autodump_enabled() {
        return;
    }
    let path = default_dump_path();
    let _ = crate::dump::dump_capacity_stats(&path);
}
