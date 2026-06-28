use crate::registry;

/// A `scc::TreeIndex<K, V>` wrapper that records creation count and peak
/// occupancy.
///
/// `TreeIndex` has no `capacity()` concept and no `with_capacity` constructor.
/// The cap hint is accepted for API uniformity but ignored.  Peak is measured
/// via `len()` on Drop (O(N) — telemetry only).
pub struct TrackedSccTreeIndex<K, V>
where
    K: Clone + Ord + 'static,
    V: Clone + 'static,
{
    inner: scc::TreeIndex<K, V>,
    name: &'static str,
}

impl<K: Clone + Ord + 'static, V: Clone + 'static> TrackedSccTreeIndex<K, V> {
    pub fn new_named(_cap_hint: usize, name: &'static str) -> Self {
        registry::record_creation(name);
        Self {
            inner: scc::TreeIndex::new(),
            name,
        }
    }
}

impl<K: Clone + Ord + 'static, V: Clone + 'static> std::ops::Deref for TrackedSccTreeIndex<K, V> {
    type Target = scc::TreeIndex<K, V>;
    fn deref(&self) -> &scc::TreeIndex<K, V> {
        &self.inner
    }
}

impl<K: Clone + Ord + 'static, V: Clone + 'static> std::ops::DerefMut
    for TrackedSccTreeIndex<K, V>
{
    fn deref_mut(&mut self) -> &mut scc::TreeIndex<K, V> {
        &mut self.inner
    }
}

impl<K: Clone + Ord + 'static, V: Clone + 'static> Drop for TrackedSccTreeIndex<K, V> {
    fn drop(&mut self) {
        // O(N) ack: telemetry only — scc::TreeIndex::len() is a full traversal.
        #[allow(clippy::disallowed_methods)]
        let peak = self.inner.len();
        registry::record_peak(self.name, peak);
    }
}
