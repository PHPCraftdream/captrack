use std::hash::{BuildHasher, Hash};

use crate::registry;
use crate::IntoInner;

/// A `scc::HashSet<T, S>` wrapper that records creation count and capacity
/// samples.
///
/// `S` defaults to `crate::CapHasher`.
///
/// # Samples record `len()`, NOT peak occupancy
///
/// `scc::HashSet` does not expose a `capacity()` method.  On every `Drop` (or
/// `From` conversion), `inner.len()` is pushed as the sample — this is the
/// element count **at the moment of Drop**, not the maximum ever observed.
///
/// If entries are removed before Drop, the recorded sample **undercounts** the
/// true peak.  Use explicit `len()` snapshots at known peak points if accurate
/// peak tracking is required for shrinking sets.
///
/// `scc::HashSet::len()` is O(N) — only called on Drop (telemetry path).
pub struct TrackedSccHashSet<T, S = crate::CapHasher>
where
    T: Eq + Hash + 'static,
    S: BuildHasher,
{
    inner: scc::HashSet<T, S>,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl<T: Eq + Hash + 'static, S: BuildHasher + Default> TrackedSccHashSet<T, S> {
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
            inner: scc::HashSet::with_capacity_and_hasher(cap, S::default()),
            name,
            file,
            line,
            column,
        }
    }
}

impl<T: Eq + Hash + 'static, S: BuildHasher> TrackedSccHashSet<T, S> {
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
            inner: scc::HashSet::with_capacity_and_hasher(cap, hasher),
            name,
            file,
            line,
            column,
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
        registry::record_sample(self.file, self.line, self.column, peak);
    }
}

// `Default` is no longer needed — ptr::read replaces mem::replace.
impl<T: Eq + Hash + 'static, S: BuildHasher> From<TrackedSccHashSet<T, S>> for scc::HashSet<T, S> {
    fn from(tracked: TrackedSccHashSet<T, S>) -> scc::HashSet<T, S> {
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

impl<T: Eq + Hash + 'static, S: BuildHasher> IntoInner for TrackedSccHashSet<T, S> {
    type Inner = scc::HashSet<T, S>;
    #[inline]
    fn into_inner(self) -> scc::HashSet<T, S> {
        scc::HashSet::from(self)
    }
}
