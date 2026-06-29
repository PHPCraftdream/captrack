// Auto-dump capacity statistics during and after process execution.
//
// Compiled only with the `telemetry` feature.  Two cooperating hooks ensure
// the global registry survives whatever termination strategy the host
// uses (graceful exit, hard kill, `timeout`-induced `TerminateProcess`):
//
//   1. **Periodic background thread** (registered via `#[ctor::ctor]`)
//      writes a fresh snapshot every `CAPTRACK_AUTODUMP_INTERVAL_MS`
//      milliseconds (default 2 000 ms).  The most recent snapshot survives
//      even when the process is killed with `SIGKILL` / Windows
//      `TerminateProcess` — both bypass atexit handlers but the on-disk
//      file from the previous tick is intact.
//
//   2. **Atexit destructor** (registered via `#[ctor::dtor]`) writes the
//      final snapshot when the process exits normally — this catches data
//      that accumulated in the last sub-interval window before exit.
//
// Criterion-driven benches never expose their `main` directly, so this is
// the only practical way to capture their data without source patches.
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
// # Configuration
//
// * `CAPTRACK_AUTODUMP=0|off|false|no` — disable both hooks entirely.
// * `CAPTRACK_AUTODUMP_INTERVAL_MS=<u64>` — periodic-thread interval
//   (default 2 000).  Set to `0` to disable the background thread (only the
//   atexit destructor runs).
// * `CAPTRACK_DUMP_DIR=<path>` — override output directory.
//
// Manual `dump_capacity_stats(path)` calls always work regardless of the
// environment.

use std::path::PathBuf;
use std::time::Duration;

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

/// Periodic-dump interval in milliseconds.  `0` → background thread is not
/// spawned; the atexit destructor is the only writer.  Default: 500 ms — a
/// compromise between I/O overhead and how much data can be lost when the
/// host process is killed mid-tick.  At 500 ms the worst-case loss window
/// is half a second on POSIX `SIGKILL` / Windows `TerminateProcess`.
fn autodump_interval_ms() -> u64 {
    std::env::var("CAPTRACK_AUTODUMP_INTERVAL_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(500)
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

/// Spawn the periodic-dump background thread at process start.  Wired via
/// `#[ctor::ctor]` so it begins ticking before `main` is entered — the
/// thread sleeps `interval_ms` between writes, so the first snapshot
/// appears `interval_ms` after process start (long enough that the registry
/// has typically seen its first allocations).
///
/// The thread is `JoinHandle`-less and runs detached for the entire process
/// lifetime; the runtime's normal teardown reaps it.  We never `.join()` it
/// because a stuck I/O write should not block process exit.
#[ctor::ctor]
fn captrack_autodump_spawn_ticker() {
    if !autodump_enabled() {
        return;
    }
    let interval_ms = autodump_interval_ms();
    if interval_ms == 0 {
        return;
    }
    let path = default_dump_path();
    std::thread::Builder::new()
        .name("captrack-autodump".into())
        .spawn(move || {
            let interval = Duration::from_millis(interval_ms);
            loop {
                std::thread::sleep(interval);
                // Errors deliberately swallowed: a transient write failure
                // (e.g. disk full, antivirus lock) must not crash the host.
                // The next tick re-attempts.
                let _ = crate::dump::dump_capacity_stats(&path);
            }
        })
        // If the OS refuses to spawn (unrealistic in practice), the periodic
        // path silently degrades to atexit-only — the dtor still fires on
        // normal exit.
        .ok();
}
