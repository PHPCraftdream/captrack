// Capacity-telemetry global registry — only compiled with the `telemetry`
// feature.
//
// Stores per call-site location:
//   • creation_count — total number of instances created (AtomicU64).
//   • samples        — raw capacity/len recorded on every Drop or into_iter.
//
// The registry is a lock-free `scc::HashMap` keyed by (file, line, column) so
// that each call-site in source code = one distinct entry.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Per-location capacity statistics accumulated over the lifetime of the process.
pub struct CapStats {
    pub name: &'static str, // fixed at first insert, never changed
    pub creation_count: AtomicU64,
    /// One sample per Drop/into_iter call — raw capacities for post-processing.
    pub samples: Mutex<Vec<usize>>,
}

impl CapStats {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            creation_count: AtomicU64::new(0),
            samples: Mutex::new(Vec::new()),
        }
    }
}

/// Location key: (file, line, column) — uniquely identifies a call-site.
pub type Loc = (&'static str, u32, u32);

// The registry uses std RandomState for its own internal map — this is an
// implementation detail, not exposed to callers, and is not performance
// critical (registry operations are only on creation/drop paths).
type Registry = scc::HashMap<Loc, CapStats>;

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// Return a reference to the process-global registry, initialising it on
/// first call.
pub fn registry() -> &'static Registry {
    REGISTRY.get_or_init(scc::HashMap::new)
}

/// Record one new creation for the call-site identified by (file, line, column).
/// Called from every `with_capacity_named` / `new_named` constructor.
/// Thread-safe: the scc map is lock-free; the counter update is `Relaxed`.
pub fn record_creation(name: &'static str, file: &'static str, line: u32, column: u32) {
    let key = (file, line, column);
    let reg = registry();
    // Fast-path: entry already present.
    if let Some(entry) = reg.get(&key) {
        entry.creation_count.fetch_add(1, Ordering::Relaxed);
        return;
    }
    // Slow-path: first time we see this location.  Two threads may race here;
    // scc::HashMap::insert returns Err if the key is already present (the
    // other thread won the race) — both outcomes are correct.
    let _ = reg.insert(key, CapStats::new(name));
    // After insert (whether ours or the other thread's) the entry exists.
    if let Some(entry) = reg.get(&key) {
        entry.creation_count.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record a capacity sample for the call-site. Called from every `Drop` impl
/// and `IntoIterator::into_iter` impl.
/// Mutex poison is ignored — telemetry is best-effort.
pub fn record_sample(file: &'static str, line: u32, column: u32, cap: usize) {
    if let Some(entry) = registry().get(&(file, line, column)) {
        if let Ok(mut v) = entry.samples.lock() {
            v.push(cap);
        }
        // If the mutex is poisoned we silently skip — telemetry is best-effort.
    }
    // If the entry is somehow absent (can only happen if record_creation was
    // never called for this location, which violates our contract) we silently
    // ignore.
}
