// Capacity-telemetry global registry — only compiled with the `telemetry`
// feature.
//
// Stores per call-site location:
//   • creation_count — total number of instances **constructed** at this site
//                      (AtomicU64).  Incremented exactly once per construction
//                      via `record_creation` or `record_initial`.
//   • samples        — bounded reservoir of capacity/len observations (Vitter
//                      Algorithm R, default 4096).  Updated on every Drop,
//                      into_iter, or cap_inspect call via `record_sample`.
//
// # Semantic invariants
//
//   creation_count  = number of times the type was constructed at this site.
//   total_observed  = samples.total_observed() = total capacity observations
//                     (every Drop / into_iter / cap_inspect call increments this).
//
// For a **safe-only** binding (wrap_from + only safe usages like push / len /
// &v + Drop):
//   creation_count == 1, total_observed == 1 (Drop sample only).
//
// For a **mixed** binding (1 wrap_from + N cap_inspect consumption points + Drop):
//   creation_count == 1, total_observed >= N + 1.
//
// For a **t*_owned!** binding (returns a bare type — Drop is NOT tracked):
//   creation_count == 1, total_observed == 1 (construction sample only).
//
// The registry is a lock-free `scc::HashMap` keyed by (file, line, column) so
// that each call-site in source code = one distinct entry.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use crate::reservoir::Reservoir;

/// Per-location capacity statistics accumulated over the lifetime of the process.
///
/// # Semantic invariants
///
/// `creation_count` is incremented exactly once per construction event
/// (`wrap_from`, `with_capacity_named`, `t*!` / `t*_owned!` macros).
/// `cap_inspect_at` (Phase L injection at consumption points) does **not**
/// touch `creation_count`.
///
/// `samples.total_observed()` counts every call to `record_sample` — one
/// per `Drop` / `into_iter` / `cap_inspect_at`.
///
/// Typical patterns:
///
/// | Binding kind              | `creation_count` | `total_observed`      |
/// |---------------------------|------------------|-----------------------|
/// | safe-only (wrap_from + Drop) | 1             | 1 (Drop sample)       |
/// | mixed (wrap_from + N inspects + Drop) | 1    | N + 1                 |
/// | t*_owned! (no Drop tracking) | 1             | 1 (initial sample)    |
///
/// `creation_count` can exceed `samples.total_observed()` when instances
/// are in-flight (created but not yet dropped), leaked via
/// `std::mem::forget`, or when Drop panics before `record_sample` is
/// reached.  The difference is an indicator of outstanding live instances.
///
/// `samples.snapshot()` returns at most `CAPTRACK_SAMPLE_CAP` (default 4096)
/// values — a statistically representative reservoir of all observed capacities.
pub struct CapStats {
    pub name: &'static str, // fixed at first insert, never changed
    pub creation_count: AtomicU64,
    /// Bounded reservoir of capacity samples (Vitter Algorithm R).
    /// Use `samples.record(cap)` to add, `samples.snapshot()` to read,
    /// `samples.total_observed()` for the full count.
    pub samples: Reservoir,
}

impl CapStats {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            creation_count: AtomicU64::new(0),
            samples: Reservoir::new(),
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

/// Record both creation AND an initial capacity sample for a call-site in
/// one call.  Used by `t*_owned!` macros that return bare types — there is
/// no `Tracked*` wrapper to capture the Drop-time capacity, so the only
/// sample is the cap requested at construction.
///
/// Equivalent to calling `record_creation` followed by `record_sample`.
pub fn record_initial(name: &'static str, file: &'static str, line: u32, column: u32, cap: usize) {
    record_creation(name, file, line, column);
    record_sample(file, line, column, cap);
}

/// Record a capacity sample for the call-site.
///
/// Called from every `Drop` impl, `IntoIterator::into_iter` impl, and
/// `CapInspect::cap_inspect_at` impl (Phase L injection).
///
/// If the call-site key is not in the registry (e.g. `cap_inspect_at` is
/// called for a construction site that was never registered via
/// `record_creation` / `record_initial`), the sample is silently discarded
/// in release mode.  In debug mode a `debug_assert` fires to surface the
/// orphan call.
///
/// Does **not** touch `creation_count`.
pub fn record_sample(file: &'static str, line: u32, column: u32, cap: usize) {
    let entry = registry().get(&(file, line, column));
    debug_assert!(
        entry.is_some(),
        "record_sample called for unregistered location {file}:{line}:{column}"
    );
    if let Some(entry) = entry {
        entry.samples.record(cap);
    }
}
