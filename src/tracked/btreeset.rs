use std::collections::BTreeSet;

use crate::registry;
use crate::IntoInner;

/// A `BTreeSet<T>` wrapper that records creation count and peak occupancy.
///
/// Like `TrackedBTreeMap`, the capacity hint is accepted but ignored for the
/// inner allocation, and peak is measured via `len()` on Drop.
pub struct TrackedBTreeSet<T: Ord> {
    inner: BTreeSet<T>,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl<T: Ord> TrackedBTreeSet<T> {
    pub fn new_named(
        _cap_hint: usize,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        registry::record_creation(name, file, line, column);
        Self {
            inner: BTreeSet::new(),
            name,
            file,
            line,
            column,
        }
    }
}

impl<T: Ord> std::ops::Deref for TrackedBTreeSet<T> {
    type Target = BTreeSet<T>;
    fn deref(&self) -> &BTreeSet<T> {
        &self.inner
    }
}

impl<T: Ord> std::ops::DerefMut for TrackedBTreeSet<T> {
    fn deref_mut(&mut self) -> &mut BTreeSet<T> {
        &mut self.inner
    }
}

impl<T: Ord> Drop for TrackedBTreeSet<T> {
    fn drop(&mut self) {
        registry::record_sample(self.file, self.line, self.column, self.inner.len());
    }
}

impl<T: Ord> From<TrackedBTreeSet<T>> for BTreeSet<T> {
    fn from(tracked: TrackedBTreeSet<T>) -> BTreeSet<T> {
        registry::record_sample(
            tracked.file,
            tracked.line,
            tracked.column,
            tracked.inner.len(),
        );
        // SAFETY: `tracked` is owned and forgotten below; ptr::read bit-copies `inner`.
        let inner = unsafe { std::ptr::read(&tracked.inner) };
        std::mem::forget(tracked);
        inner
    }
}

impl<T: Ord> IntoInner for TrackedBTreeSet<T> {
    type Inner = BTreeSet<T>;
    #[inline]
    fn into_inner(self) -> BTreeSet<T> {
        BTreeSet::from(self)
    }
}

impl<T: Ord> IntoIterator for TrackedBTreeSet<T> {
    type Item = T;
    type IntoIter = std::collections::btree_set::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        registry::record_sample(self.file, self.line, self.column, self.inner.len());
        // SAFETY: `self` is owned and forgotten below; ptr::read bit-copies `inner`.
        let inner = unsafe { std::ptr::read(&self.inner) };
        std::mem::forget(self);
        inner.into_iter()
    }
}
