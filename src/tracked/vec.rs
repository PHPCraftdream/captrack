use crate::registry;

/// A `Vec<T>` wrapper that records creation count and capacity samples in the
/// global capacity-telemetry registry.
///
/// Compiled only when the `telemetry` feature is enabled; in off-feature mode
/// the `tvec!` macro expands directly to `::std::vec::Vec::with_capacity(cap)`
/// with zero overhead.
pub struct TrackedVec<T> {
    inner: Vec<T>,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl<T> TrackedVec<T> {
    /// Create a new `TrackedVec` with the given capacity and register the
    /// creation in the global registry.
    pub fn with_capacity_named(
        cap: usize,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        registry::record_creation(name, file, line, column);
        Self {
            inner: Vec::with_capacity(cap),
            name,
            file,
            line,
            column,
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
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
    }
}

// Cross-feature boundary bridge: converting `TrackedVec<T>` to bare `Vec<T>` at
// API boundaries.  Records the final capacity sample (same as Drop would) and
// then unwraps `inner`.  Pattern mirrors `IntoIterator::into_iter` below:
// `record_sample` BEFORE `mem::take`, then `mem::forget(self)` so Drop doesn't
// run again and record `capacity() == 0`.
impl<T> From<TrackedVec<T>> for Vec<T> {
    fn from(mut tracked: TrackedVec<T>) -> Vec<T> {
        registry::record_sample(
            tracked.file,
            tracked.line,
            tracked.column,
            tracked.inner.capacity(),
        );
        let inner = std::mem::take(&mut tracked.inner);
        std::mem::forget(tracked);
        inner
    }
}

impl<T> IntoIterator for TrackedVec<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(mut self) -> Self::IntoIter {
        // Record sample before consuming `inner`.  We must do this explicitly
        // here because `into_iter` moves out of `self.inner` via
        // `std::mem::take`, leaving behind an empty `Vec` — if we let `Drop`
        // run afterwards it would see `capacity() == 0` and record a false
        // zero.  `std::mem::forget(self)` prevents the Drop from running a
        // second time.
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
        let inner = std::mem::take(&mut self.inner);
        std::mem::forget(self);
        inner.into_iter()
    }
}
