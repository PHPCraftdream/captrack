// dump_capacity_stats — always available, no-op in off-feature mode.
//
// In on-feature mode: serialises the global registry to a pretty-printed JSON
// file, sorted by max(samples) descending so the biggest allocations surface
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
        file: &'static str,
        line: u32,
        column: u32,
        creation_count: u64,
        samples: Vec<usize>,
    }

    #[derive(Serialize)]
    struct Dump {
        version: u32,
        stats: Vec<Entry>,
    }

    fn entry_from((file, line, column): (&'static str, u32, u32), stats: &CapStats) -> Entry {
        // scc::Bag has no non-destructive shared-reference iterator, so we drain
        // via pop_all and push all values back to leave the bag intact between dumps.
        let samples: Vec<usize> = stats.samples.pop_all(Vec::new(), |mut v, x| {
            v.push(x);
            v
        });
        for &s in &samples {
            stats.samples.push(s);
        }
        Entry {
            name: stats.name,
            file,
            line,
            column,
            creation_count: stats.creation_count.load(Ordering::Relaxed),
            samples,
        }
    }

    pub fn dump_capacity_stats(path: impl AsRef<Path>) -> std::io::Result<()> {
        let mut entries: Vec<Entry> = Vec::new();
        crate::registry::registry().scan(|loc, stats| {
            entries.push(entry_from(*loc, stats));
        });
        // Sort by max sample descending — entries with no samples sort last (0).
        entries.sort_by_key(|e| Reverse(e.samples.iter().copied().max().unwrap_or(0)));

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
/// `max(samples)` descending.
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
