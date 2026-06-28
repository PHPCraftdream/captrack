use std::hash::{BuildHasher, Hash};

use crate::registry;

/// A `scc::HashSet<T, S>` wrapper that records creation count and peak
/// occupancy.
///
/// `S` defaults to `crate::CapHasher`.
///
/// `scc::HashSet::len()` is O(N) — only called on Drop (telemetry path).
pub struct TrackedSccHashSet<T, S = crate::CapHasher>
where
    T: Eq + Hash + 'static,
    S: BuildHasher,
{
    inner: scc::HashSet<T, S>,
    name: &'static str,
}

impl<T: Eq + Hash + 'static, S: BuildHasher + Default> TrackedSccHashSet<T, S> {
    /// Create with the default hasher (`S::default()`).
    pub fn with_capacity_named(cap: usize, name: &'static str) -> Self {
        registry::record_creation(name);
        Self {
            inner: scc::HashSet::with_capacity_and_hasher(cap, S::default()),
            name,
        }
    }
}

impl<T: Eq + Hash + 'static, S: BuildHasher> TrackedSccHashSet<T, S> {
    /// Create with an explicit hasher instance (per-call override, Axis 2B).
    pub fn with_capacity_and_hasher_named(cap: usize, hasher: S, name: &'static str) -> Self {
        registry::record_creation(name);
        Self {
            inner: scc::HashSet::with_capacity_and_hasher(cap, hasher),
            name,
        }
    }
}

impl<T: Eq + Hash + 'static, S: BuildHasher> std::ops::Deref for TrackedSccHashSet<T, S> {
    type Target = scc::HashSet<T, S>;
    fn deref(&self) -> &scc::HashSet<T, S> {
        &self.inner
    }
}

impl<T: Eq + Hash + 'static, S: BuildHasher> std::ops::DerefMut for TrackedSccHashSet<T, S> {
    fn deref_mut(&mut self) -> &mut scc::HashSet<T, S> {
        &mut self.inner
    }
}

impl<T: Eq + Hash + 'static, S: BuildHasher> Drop for TrackedSccHashSet<T, S> {
    fn drop(&mut self) {
        // O(N) ack: telemetry only — scc::HashSet::len() is a full traversal.
        #[allow(clippy::disallowed_methods)]
        let peak = self.inner.len();
        registry::record_peak(self.name, peak);
    }
}
