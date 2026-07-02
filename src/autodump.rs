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

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serializes tests that mutate process-wide environment variables —
    // `cargo test` runs tests in the same binary concurrently by default, and
    // std::env mutation is process-global.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── autodump_interval_ms ─────────────────────────────────────────────────

    #[test]
    fn interval_defaults_to_500ms_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("CAPTRACK_AUTODUMP_INTERVAL_MS");
        std::env::remove_var("CAPTRACK_AUTODUMP_INTERVAL_MS");

        assert_eq!(autodump_interval_ms(), 500);

        if let Some(v) = prev {
            std::env::set_var("CAPTRACK_AUTODUMP_INTERVAL_MS", v);
        }
    }

    #[test]
    fn interval_respects_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("CAPTRACK_AUTODUMP_INTERVAL_MS");
        std::env::set_var("CAPTRACK_AUTODUMP_INTERVAL_MS", "123");

        assert_eq!(autodump_interval_ms(), 123);

        match prev {
            Some(v) => std::env::set_var("CAPTRACK_AUTODUMP_INTERVAL_MS", v),
            None => std::env::remove_var("CAPTRACK_AUTODUMP_INTERVAL_MS"),
        }
    }

    #[test]
    fn interval_falls_back_to_default_on_unparseable_value() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("CAPTRACK_AUTODUMP_INTERVAL_MS");
        std::env::set_var("CAPTRACK_AUTODUMP_INTERVAL_MS", "not-a-number");

        assert_eq!(autodump_interval_ms(), 500);

        match prev {
            Some(v) => std::env::set_var("CAPTRACK_AUTODUMP_INTERVAL_MS", v),
            None => std::env::remove_var("CAPTRACK_AUTODUMP_INTERVAL_MS"),
        }
    }

    // ── autodump_enabled ─────────────────────────────────────────────────────

    #[test]
    fn enabled_by_default_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("CAPTRACK_AUTODUMP");
        std::env::remove_var("CAPTRACK_AUTODUMP");

        assert!(autodump_enabled());

        if let Some(v) = prev {
            std::env::set_var("CAPTRACK_AUTODUMP", v);
        }
    }

    #[test]
    fn disabled_by_recognized_falsy_values() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("CAPTRACK_AUTODUMP");

        for falsy in ["0", "off", "false", "no", "OFF", "False"] {
            std::env::set_var("CAPTRACK_AUTODUMP", falsy);
            assert!(!autodump_enabled(), "expected {falsy} to disable autodump");
        }

        match prev {
            Some(v) => std::env::set_var("CAPTRACK_AUTODUMP", v),
            None => std::env::remove_var("CAPTRACK_AUTODUMP"),
        }
    }

    // ── default_dump_path ────────────────────────────────────────────────────

    #[test]
    fn dump_path_respects_captrack_dump_dir_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("CAPTRACK_DUMP_DIR");
        std::env::set_var("CAPTRACK_DUMP_DIR", "some/custom/dir");

        let path = default_dump_path();
        assert!(path.starts_with("some/custom/dir"));
        assert!(path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("profile-"));

        match prev {
            Some(v) => std::env::set_var("CAPTRACK_DUMP_DIR", v),
            None => std::env::remove_var("CAPTRACK_DUMP_DIR"),
        }
    }

    // ── end-to-end: dump format + atomic write (via dump::dump_capacity_stats,
    //    the same function the periodic ticker and the atexit dtor call) ─────

    #[test]
    fn dump_output_is_atomic_and_leaves_no_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile-test.json");

        crate::dump::dump_capacity_stats(&path).unwrap();

        assert!(path.is_file(), "dump file must exist after a successful write");

        let tmp_path = {
            let mut name = path.file_name().unwrap().to_os_string();
            name.push(".tmp");
            path.with_file_name(name)
        };
        assert!(!tmp_path.exists(), "no .tmp file must remain after a successful write");
    }

    #[test]
    fn dump_output_contains_total_observed_field() {
        use crate::tvec;

        // Force at least one registry entry so the dump has content.
        let v: crate::TrackedVec<i32> = tvec!("autodump-test-total-observed", 4);
        drop(v);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile-total-observed.json");
        crate::dump::dump_capacity_stats(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("total_observed"),
            "dump JSON must include total_observed; got: {text}"
        );
    }

    #[test]
    fn dump_does_not_fail_on_empty_registry() {
        // A path under a fresh tempdir guarantees no other test has written
        // to the same file; the registry itself may or may not be empty
        // depending on test execution order, but the call must succeed and
        // produce valid JSON regardless.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile-empty.json");

        let result = crate::dump::dump_capacity_stats(&path);
        assert!(result.is_ok());
        assert!(path.is_file());

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(parsed.get("stats").is_some());
    }
}
