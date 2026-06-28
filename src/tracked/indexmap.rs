use std::hash::{BuildHasher, Hash};

use indexmap::IndexMap;

use crate::registry;

/// A `IndexMap<K, V, S>` wrapper — insertion-ordered map with telemetry.
///
/// `S` defaults to `crate::CapHasher`.
pub struct TrackedIndexMap<K, V, S = crate::CapHasher> {
    inner: IndexMap<K, V, S>,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl<K: Eq + Hash, V, S: BuildHasher + Default> TrackedIndexMap<K, V, S> {
    /// Create with the default hasher (`S::default()`).
    pub fn with_capacity_named(
        cap: usize,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        registry::record_creation(name, file, line, column);
        Self {
            inner: IndexMap::with_capacity_and_hasher(cap, S::default()),
            name,
            file,
            line,
            column,
        }
    }
}

impl<K: Eq + Hash, V, S: BuildHasher> TrackedIndexMap<K, V, S> {
    /// Create with an explicit hasher instance (per-call override, Axis 2B).
    pub fn with_capacity_and_hasher_named(
        cap: usize,
        hasher: S,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        registry::record_creation(name, file, line, column);
        Self {
            inner: IndexMap::with_capacity_and_hasher(cap, hasher),
            name,
            file,
            line,
            column,
        }
    }
}

impl<K, V, S> std::ops::Deref for TrackedIndexMap<K, V, S> {
    type Target = IndexMap<K, V, S>;
    fn deref(&self) -> &IndexMap<K, V, S> {
        &self.inner
    }
}

impl<K, V, S> std::ops::DerefMut for TrackedIndexMap<K, V, S> {
    fn deref_mut(&mut self) -> &mut IndexMap<K, V, S> {
        &mut self.inner
    }
}

impl<K, V, S> Drop for TrackedIndexMap<K, V, S> {
    fn drop(&mut self) {
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
    }
}

impl<K: Eq + Hash, V, S: BuildHasher + Default> From<TrackedIndexMap<K, V, S>>
    for IndexMap<K, V, S>
{
    fn from(mut tracked: TrackedIndexMap<K, V, S>) -> IndexMap<K, V, S> {
        registry::record_sample(
            tracked.file,
            tracked.line,
            tracked.column,
            tracked.inner.capacity(),
        );
        let inner = std::mem::replace(&mut tracked.inner, IndexMap::with_hasher(S::default()));
        std::mem::forget(tracked);
        inner
    }
}

impl<K: Eq + Hash, V, S: BuildHasher + Default> IntoIterator for TrackedIndexMap<K, V, S> {
    type Item = (K, V);
    type IntoIter = indexmap::map::IntoIter<K, V>;

    fn into_iter(mut self) -> Self::IntoIter {
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
        let inner = std::mem::replace(&mut self.inner, IndexMap::with_hasher(S::default()));
        std::mem::forget(self);
        inner.into_iter()
    }
}
