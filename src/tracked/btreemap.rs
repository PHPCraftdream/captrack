use std::collections::BTreeMap;

use crate::registry;

/// A `BTreeMap<K, V>` wrapper that records creation count and peak occupancy.
///
/// `BTreeMap` has no `with_capacity` so the capacity hint passed to the macro
/// is accepted but ignored for the inner allocation.  In Drop, `inner.len()`
/// is used as the peak metric (B-tree capacity is not observable).
pub struct TrackedBTreeMap<K: Ord, V> {
    inner: BTreeMap<K, V>,
    name: &'static str,
}

impl<K: Ord, V> TrackedBTreeMap<K, V> {
    /// `_cap_hint` is accepted for API uniformity (matches the macro signature)
    /// but is not passed to `BTreeMap` which has no capacity concept.
    pub fn new_named(_cap_hint: usize, name: &'static str) -> Self {
        registry::record_creation(name);
        Self {
            inner: BTreeMap::new(),
            name,
        }
    }
}

impl<K: Ord, V> std::ops::Deref for TrackedBTreeMap<K, V> {
    type Target = BTreeMap<K, V>;
    fn deref(&self) -> &BTreeMap<K, V> {
        &self.inner
    }
}

impl<K: Ord, V> std::ops::DerefMut for TrackedBTreeMap<K, V> {
    fn deref_mut(&mut self) -> &mut BTreeMap<K, V> {
        &mut self.inner
    }
}

impl<K: Ord, V> Drop for TrackedBTreeMap<K, V> {
    fn drop(&mut self) {
        // BTreeMap has no `capacity()` — record len as peak metric.
        registry::record_peak(self.name, self.inner.len());
    }
}

impl<K: Ord, V> IntoIterator for TrackedBTreeMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::collections::btree_map::IntoIter<K, V>;

    fn into_iter(mut self) -> Self::IntoIter {
        registry::record_peak(self.name, self.inner.len());
        let inner = std::mem::take(&mut self.inner);
        std::mem::forget(self);
        inner.into_iter()
    }
}
