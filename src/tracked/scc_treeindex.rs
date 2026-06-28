use crate::registry;
use crate::IntoInner;

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
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl<K: Clone + Ord + 'static, V: Clone + 'static> TrackedSccTreeIndex<K, V> {
    pub fn new_named(
        _cap_hint: usize,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        registry::record_creation(name, file, line, column);
        Self {
            inner: scc::TreeIndex::new(),
            name,
            file,
            line,
            column,
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
        registry::record_sample(self.file, self.line, self.column, peak);
    }
}

impl<K: Clone + Ord + 'static, V: Clone + 'static> From<TrackedSccTreeIndex<K, V>>
    for scc::TreeIndex<K, V>
{
    fn from(tracked: TrackedSccTreeIndex<K, V>) -> scc::TreeIndex<K, V> {
        #[allow(clippy::disallowed_methods)]
        let peak = tracked.inner.len();
        registry::record_sample(tracked.file, tracked.line, tracked.column, peak);
        // SAFETY: `tracked` is owned and forgotten below; ptr::read bit-copies `inner`.
        let inner = unsafe { std::ptr::read(&tracked.inner) };
        std::mem::forget(tracked);
        inner
    }
}

impl<K: Clone + Ord + 'static, V: Clone + 'static> IntoInner for TrackedSccTreeIndex<K, V> {
    type Inner = scc::TreeIndex<K, V>;
    #[inline]
    fn into_inner(self) -> scc::TreeIndex<K, V> {
        scc::TreeIndex::from(self)
    }
}
