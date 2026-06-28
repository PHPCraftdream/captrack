use std::collections::BTreeSet;

use crate::registry;

/// A `BTreeSet<T>` wrapper that records creation count and peak occupancy.
///
/// Like `TrackedBTreeMap`, the capacity hint is accepted but ignored for the
/// inner allocation, and peak is measured via `len()` on Drop.
pub struct TrackedBTreeSet<T: Ord> {
    inner: BTreeSet<T>,
    name: &'static str,
}

impl<T: Ord> TrackedBTreeSet<T> {
    pub fn new_named(_cap_hint: usize, name: &'static str) -> Self {
        registry::record_creation(name);
        Self {
            inner: BTreeSet::new(),
            name,
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
        registry::record_peak(self.name, self.inner.len());
    }
}

impl<T: Ord> IntoIterator for TrackedBTreeSet<T> {
    type Item = T;
    type IntoIter = std::collections::btree_set::IntoIter<T>;

    fn into_iter(mut self) -> Self::IntoIter {
        registry::record_peak(self.name, self.inner.len());
        let inner = std::mem::take(&mut self.inner);
        std::mem::forget(self);
        inner.into_iter()
    }
}
