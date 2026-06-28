use crate::registry;

/// A `Vec<T>` wrapper that records creation count and peak capacity in the
/// global capacity-telemetry registry.
///
/// Compiled only when the `telemetry` feature is enabled; in off-feature mode
/// the `tvec!` macro expands directly to `::std::vec::Vec::with_capacity(cap)`
/// with zero overhead.
pub struct TrackedVec<T> {
    inner: Vec<T>,
    name: &'static str,
}

impl<T> TrackedVec<T> {
    /// Create a new `TrackedVec` with the given capacity and register the
    /// creation in the global registry.
    pub fn with_capacity_named(cap: usize, name: &'static str) -> Self {
        registry::record_creation(name);
        Self {
            inner: Vec::with_capacity(cap),
            name,
        }
    }
}

impl<T> std::ops::Deref for TrackedVec<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        &self.inner
    }
}

impl<T> std::ops::DerefMut for TrackedVec<T> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        &mut self.inner
    }
}

impl<T> Drop for TrackedVec<T> {
    fn drop(&mut self) {
        registry::record_peak(self.name, self.inner.capacity());
    }
}

impl<T> IntoIterator for TrackedVec<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(mut self) -> Self::IntoIter {
        // Record peak before consuming `inner`.  We must do this explicitly
        // here because `into_iter` moves out of `self.inner` via
        // `std::mem::take`, leaving behind an empty `Vec` — if we let `Drop`
        // run afterwards it would see `capacity() == 0` and record a false
        // zero.  `std::mem::forget(self)` prevents the Drop from running a
        // second time.
        registry::record_peak(self.name, self.inner.capacity());
        let inner = std::mem::take(&mut self.inner);
        std::mem::forget(self);
        inner.into_iter()
    }
}
