use crate::registry;
use crate::IntoInner;

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
// then unwraps `inner`.
impl<T> From<TrackedVec<T>> for Vec<T> {
    fn from(tracked: TrackedVec<T>) -> Vec<T> {
        registry::record_sample(
            tracked.file,
            tracked.line,
            tracked.column,
            tracked.inner.capacity(),
        );
        // SAFETY: `tracked` is owned by us and will be forgotten on the next
        // line, so its Drop never runs.  `ptr::read` bit-copies `inner` out;
        // ownership moves to the returned value.
        let inner = unsafe { std::ptr::read(&tracked.inner) };
        std::mem::forget(tracked);
        inner
    }
}

impl<T> IntoInner for TrackedVec<T> {
    type Inner = Vec<T>;
    #[inline]
    fn into_inner(self) -> Vec<T> {
        Vec::from(self)
    }
}

impl<T> IntoIterator for TrackedVec<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        // Record sample before consuming `inner`.  We must do this explicitly
        // here because `into_iter` moves out of `self.inner` — if we let `Drop`
        // run afterwards it would see `capacity() == 0` and record a false
        // zero.  `std::mem::forget(self)` prevents the Drop from running a
        // second time.
        //
        // SAFETY: `self` is owned by us and will be forgotten on the next
        // line, so its Drop never runs.  `ptr::read` bit-copies `inner` out.
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
        let inner = unsafe { std::ptr::read(&self.inner) };
        std::mem::forget(self);
        inner.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a TrackedVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.inner.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut TrackedVec<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.inner.iter_mut()
    }
}
