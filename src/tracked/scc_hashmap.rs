use std::hash::{BuildHasher, Hash};

use crate::registry;
use crate::IntoInner;

/// A `scc::HashMap<K, V, S>` wrapper that records creation count and peak
/// occupancy.
///
/// `S` defaults to `crate::CapHasher`.
///
/// `scc::HashMap::len()` is O(N) — only called on Drop (telemetry path).
pub struct TrackedSccHashMap<K, V, S = crate::CapHasher>
where
    K: Eq + Hash + 'static,
    V: 'static,
    S: BuildHasher,
{
    inner: scc::HashMap<K, V, S>,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl<K: Eq + Hash + 'static, V: 'static, S: BuildHasher + Default> TrackedSccHashMap<K, V, S> {
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
            inner: scc::HashMap::with_capacity_and_hasher(cap, S::default()),
            name,
            file,
            line,
            column,
        }
    }
}

impl<K: Eq + Hash + 'static, V: 'static, S: BuildHasher> TrackedSccHashMap<K, V, S> {
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
            inner: scc::HashMap::with_capacity_and_hasher(cap, hasher),
            name,
            file,
            line,
            column,
        }
    }
}

impl<K: Eq + Hash + 'static, V: 'static, S: BuildHasher> std::ops::Deref
    for TrackedSccHashMap<K, V, S>
{
    type Target = scc::HashMap<K, V, S>;
    fn deref(&self) -> &scc::HashMap<K, V, S> {
        &self.inner
    }
}

impl<K: Eq + Hash + 'static, V: 'static, S: BuildHasher> std::ops::DerefMut
    for TrackedSccHashMap<K, V, S>
{
    fn deref_mut(&mut self) -> &mut scc::HashMap<K, V, S> {
        &mut self.inner
    }
}

impl<K: Eq + Hash + 'static, V: 'static, S: BuildHasher> Drop for TrackedSccHashMap<K, V, S> {
    fn drop(&mut self) {
        // O(N) ack: telemetry only — scc::HashMap::len() is a full traversal,
        // but this only runs on Drop, not on any hot path.
        #[allow(clippy::disallowed_methods)]
        let peak = self.inner.len();
        registry::record_sample(self.file, self.line, self.column, peak);
    }
}

// `Default` is no longer needed — ptr::read replaces mem::replace.
impl<K: Eq + Hash + 'static, V: 'static, S: BuildHasher> From<TrackedSccHashMap<K, V, S>>
    for scc::HashMap<K, V, S>
{
    fn from(tracked: TrackedSccHashMap<K, V, S>) -> scc::HashMap<K, V, S> {
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

impl<K: Eq + Hash + 'static, V: 'static, S: BuildHasher> IntoInner for TrackedSccHashMap<K, V, S> {
    type Inner = scc::HashMap<K, V, S>;
    #[inline]
    fn into_inner(self) -> scc::HashMap<K, V, S> {
        scc::HashMap::from(self)
    }
}
