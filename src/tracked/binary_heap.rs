use std::collections::BinaryHeap;

use crate::registry;
use crate::IntoInner;

/// A `BinaryHeap<T>` wrapper that records creation count and capacity samples
/// in the global capacity-telemetry registry.
///
/// Compiled only when the `telemetry` feature is enabled; in off-feature mode
/// the `tbinaryheap!` macro expands directly to
/// `::std::collections::BinaryHeap::with_capacity(cap)` with zero overhead.
pub struct TrackedBinaryHeap<T: Ord> {
    inner: BinaryHeap<T>,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl<T: Ord> TrackedBinaryHeap<T> {
    /// Create a new `TrackedBinaryHeap` with the given capacity and register the
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
            inner: BinaryHeap::with_capacity(cap),
            name,
            file,
            line,
            column,
        }
    }

    /// Wrap an already-constructed `BinaryHeap<T>` for capacity telemetry.
    ///
    /// Records creation in the registry; `inner` is moved into the wrapper
    /// as-is — no reallocation occurs.  The capacity sample is recorded at
    /// `Drop` (or `From`/`IntoIterator` conversion) as usual.
    ///
    /// This is the universal instrument entry-point used by Phase K of the
    /// `captrack-pgo instrument` pass.
    #[inline]
    pub fn wrap_from(
        inner: BinaryHeap<T>,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        registry::record_creation(name, file, line, column);
        Self { inner, name, file, line, column }
    }
}

impl<T: Ord> std::ops::Deref for TrackedBinaryHeap<T> {
    type Target = BinaryHeap<T>;
    fn deref(&self) -> &BinaryHeap<T> {
        &self.inner
    }
}

impl<T: Ord> std::ops::DerefMut for TrackedBinaryHeap<T> {
    fn deref_mut(&mut self) -> &mut BinaryHeap<T> {
        &mut self.inner
    }
}

impl<T: Ord> Drop for TrackedBinaryHeap<T> {
    fn drop(&mut self) {
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
    }
}

impl<T: Ord> From<TrackedBinaryHeap<T>> for BinaryHeap<T> {
    fn from(tracked: TrackedBinaryHeap<T>) -> BinaryHeap<T> {
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

impl<T: Ord> IntoInner for TrackedBinaryHeap<T> {
    type Inner = BinaryHeap<T>;
    #[inline]
    fn into_inner(self) -> BinaryHeap<T> {
        BinaryHeap::from(self)
    }
}

impl<T: Ord> IntoIterator for TrackedBinaryHeap<T> {
    type Item = T;
    type IntoIter = std::collections::binary_heap::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        // Record sample before consuming `inner`.  We must do this explicitly
        // here because `into_iter` moves out of `self.inner` — if we let `Drop`
        // run afterwards it would see `capacity() == 0` and record a false
        // zero.  `std::mem::forget(self)` prevents the Drop from running a
        // second time.
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
        // SAFETY: `self` is owned by us and will be forgotten on the next
        // line, so its Drop never runs.  `ptr::read` bit-copies `inner` out.
        let inner = unsafe { std::ptr::read(&self.inner) };
        std::mem::forget(self);
        inner.into_iter()
    }
}

impl<T: Ord> crate::cap_inspect::CapInspect for TrackedBinaryHeap<T> {
    #[inline]
    fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
        registry::record_initial(name, file, line, column, self.inner.capacity());
    }
}
