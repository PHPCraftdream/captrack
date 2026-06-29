use std::hash::{BuildHasher, Hash};

use dashmap::DashMap;

use crate::registry;
use crate::IntoInner;

/// A `DashMap<K, V, S>` wrapper that records creation count and capacity samples.
///
/// `S` defaults to `crate::CapHasher`.
///
/// # Samples record `len()`, NOT peak occupancy
///
/// `DashMap` does not expose a `capacity()` method.  On every `Drop` (or
/// `From` conversion), `inner.len()` is pushed as the sample — this is the
/// element count **at the moment of Drop**, not the maximum ever observed.
///
/// If entries are removed from the map before it is dropped (e.g. via
/// `DashMap::remove` or `DashMap::clear`), the recorded sample **undercounts**
/// the true peak.  Use explicit `len()` snapshots at known peak points if
/// accurate peak tracking is required for shrinking maps.
///
/// # Telemetry note
///
/// `DashMap::len()` is O(N) — it iterates all shards.  This call is
/// intentionally limited to the Drop path (telemetry only, not a hot path).
pub struct TrackedDashMap<K, V, S = crate::CapHasher>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    inner: DashMap<K, V, S>,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl<K: Eq + Hash, V, S: BuildHasher + Clone + Default> TrackedDashMap<K, V, S> {
    /// Create with the default hasher (`S::default()`).
    pub fn with_capacity_named(
        cap: usize,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        registry::record_creation(name, file, line, column);
        Self {
            inner: DashMap::with_capacity_and_hasher(cap, S::default()),
            name,
            file,
            line,
            column,
        }
    }
}

impl<K: Eq + Hash, V, S: BuildHasher + Clone> TrackedDashMap<K, V, S> {
    /// Create with an explicit hasher instance (per-call override, Axis 2B).
    pub fn with_capacity_and_hasher_named(
        cap: usize,
        hasher: S,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        registry::record_creation(name, file, line, column);
        Self {
            inner: DashMap::with_capacity_and_hasher(cap, hasher),
            name,
            file,
            line,
            column,
        }
    }

    /// Wrap an already-constructed `DashMap<K, V, S>` for capacity telemetry.
    ///
    /// Records creation in the registry; `inner` is moved as-is.
    /// The sample metric at `Drop` is `inner.len()` (DashMap has no capacity()).
    #[inline]
    pub fn wrap_from(
        inner: DashMap<K, V, S>,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        registry::record_creation(name, file, line, column);
        Self { inner, name, file, line, column }
    }
}

impl<K: Eq + Hash, V, S: BuildHasher + Clone> std::ops::Deref for TrackedDashMap<K, V, S> {
    type Target = DashMap<K, V, S>;
    fn deref(&self) -> &DashMap<K, V, S> {
        &self.inner
    }
}

impl<K: Eq + Hash, V, S: BuildHasher + Clone> std::ops::DerefMut for TrackedDashMap<K, V, S> {
    fn deref_mut(&mut self) -> &mut DashMap<K, V, S> {
        &mut self.inner
    }
}

impl<K: Eq + Hash, V, S: BuildHasher + Clone> Drop for TrackedDashMap<K, V, S> {
    fn drop(&mut self) {
        // O(N) ack: telemetry only — not a hot path.
        #[allow(clippy::disallowed_methods)]
        let peak = self.inner.len();
        registry::record_sample(self.file, self.line, self.column, peak);
    }
}

// `S: Clone` is required by `DashMap`'s own struct definition (not our
// constraint). `Default` is no longer needed — ptr::read replaces mem::replace.
impl<K: Eq + Hash, V, S: BuildHasher + Clone> From<TrackedDashMap<K, V, S>> for DashMap<K, V, S> {
    fn from(tracked: TrackedDashMap<K, V, S>) -> DashMap<K, V, S> {
        #[allow(clippy::disallowed_methods)]
        let peak = tracked.inner.len();
        registry::record_sample(tracked.file, tracked.line, tracked.column, peak);
        // SAFETY: `tracked` is owned and forgotten below; ptr::read bit-copies
        // `inner` without requiring `Default` on S.
        let inner = unsafe { std::ptr::read(&tracked.inner) };
        std::mem::forget(tracked);
        inner
    }
}

impl<K: Eq + Hash, V, S: BuildHasher + Clone> IntoInner for TrackedDashMap<K, V, S> {
    type Inner = DashMap<K, V, S>;
    #[inline]
    fn into_inner(self) -> DashMap<K, V, S> {
        DashMap::from(self)
    }
}
