// dump_capacity_stats — always available, no-op in off-feature mode.
//
// In on-feature mode: serialises the global registry to a pretty-printed JSON
// file, sorted by peak_capacity descending so the biggest allocations surface
// first.
//
// Intended use: call once at the end of a benchmark main function.
//   captrack::dump_capacity_stats("target/capacity-stats/my_bench.json")?;

#[cfg(feature = "telemetry")]
mod inner {
    use std::cmp::Reverse;
    use std::path::Path;
    use std::sync::atomic::Ordering;

    use serde::Serialize;

    use crate::registry::CapStats;

    #[derive(Serialize)]
    struct Entry {
        name: &'static str,
        peak_capacity: usize,
        creation_count: u64,
    }

    #[derive(Serialize)]
    struct Dump {
        version: u32,
        stats: Vec<Entry>,
    }

    fn entry_from(name: &'static str, stats: &CapStats) -> Entry {
        Entry {
            name,
            peak_capacity: stats.peak_capacity.load(Ordering::Relaxed),
            creation_count: stats.creation_count.load(Ordering::Relaxed),
        }
    }

    pub fn dump_capacity_stats(path: impl AsRef<Path>) -> std::io::Result<()> {
        let mut entries: Vec<Entry> = Vec::new();
        crate::registry::registry().scan(|name, stats| {
            entries.push(entry_from(name, stats));
        });
        entries.sort_by_key(|e| Reverse(e.peak_capacity));

        let dump = Dump {
            version: 1,
            stats: entries,
        };
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let f = std::fs::File::create(path)?;
        serde_json::to_writer_pretty(f, &dump).map_err(std::io::Error::other)?;
        Ok(())
    }
}

/// Write accumulated capacity statistics to a JSON file, sorted by
/// `peak_capacity` descending.
///
/// In off-feature mode this is a no-op that returns `Ok(())` immediately so
/// benchmark code can call it unconditionally without `#[cfg]` guards.
///
/// # Examples
///
/// ```ignore
/// // At the end of a benchmark:
/// captrack::dump_capacity_stats("target/capacity-stats/my_bench.json")?;
/// ```
#[cfg(feature = "telemetry")]
pub use inner::dump_capacity_stats;

/// No-op stub — compiled when the `telemetry` feature is not enabled.
#[cfg(not(feature = "telemetry"))]
pub fn dump_capacity_stats<P: AsRef<std::path::Path>>(_path: P) -> std::io::Result<()> {
    Ok(())
}
