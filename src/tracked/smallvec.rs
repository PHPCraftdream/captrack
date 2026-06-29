use smallvec::SmallVec;

use crate::registry;
use crate::IntoInner;

/// A `SmallVec<A>` wrapper that records creation count and capacity samples in
/// the global capacity-telemetry registry.
///
/// Compiled only when the `telemetry` feature is enabled; in off-feature mode
/// the `tsmallvec!` macro expands directly to `::smallvec::SmallVec::with_capacity(cap)`
/// with zero overhead.
pub struct TrackedSmallVec<A: smallvec::Array> {
    inner: SmallVec<A>,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl<A: smallvec::Array> TrackedSmallVec<A> {
    /// Create a new `TrackedSmallVec` with the given capacity and register the
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
            inner: SmallVec::with_capacity(cap),
            name,
            file,
            line,
            column,
        }
    }
}

impl<A: smallvec::Array> std::ops::Deref for TrackedSmallVec<A> {
    type Target = SmallVec<A>;
    fn deref(&self) -> &SmallVec<A> {
        &self.inner
    }
}

impl<A: smallvec::Array> std::ops::DerefMut for TrackedSmallVec<A> {
    fn deref_mut(&mut self) -> &mut SmallVec<A> {
        &mut self.inner
    }
}

impl<A: smallvec::Array> Drop for TrackedSmallVec<A> {
    fn drop(&mut self) {
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
    }
}

// Cross-feature boundary bridge: converting `TrackedSmallVec<A>` to bare
// `SmallVec<A>` at API boundaries.  Records the final capacity sample (same as
// Drop would) and then unwraps `inner`.
impl<A: smallvec::Array> From<TrackedSmallVec<A>> for SmallVec<A> {
    fn from(tracked: TrackedSmallVec<A>) -> SmallVec<A> {
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

impl<A: smallvec::Array> IntoInner for TrackedSmallVec<A> {
    type Inner = SmallVec<A>;
    #[inline]
    fn into_inner(self) -> SmallVec<A> {
        SmallVec::from(self)
    }
}

impl<A: smallvec::Array> IntoIterator for TrackedSmallVec<A> {
    type Item = A::Item;
    type IntoIter = smallvec::IntoIter<A>;

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

impl<'a, A: smallvec::Array> IntoIterator for &'a TrackedSmallVec<A> {
    type Item = &'a A::Item;
    type IntoIter = std::slice::Iter<'a, A::Item>;
    fn into_iter(self) -> Self::IntoIter {
        self.inner.iter()
    }
}

impl<'a, A: smallvec::Array> IntoIterator for &'a mut TrackedSmallVec<A> {
    type Item = &'a mut A::Item;
    type IntoIter = std::slice::IterMut<'a, A::Item>;
    fn into_iter(self) -> Self::IntoIter {
        self.inner.iter_mut()
    }
}
