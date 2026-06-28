use std::hash::{BuildHasher, Hash};

use indexmap::IndexSet;

use crate::registry;
use crate::IntoInner;

/// A `IndexSet<T, S>` wrapper — insertion-ordered set with telemetry.
///
/// `S` defaults to `crate::CapHasher`.
pub struct TrackedIndexSet<T, S = crate::CapHasher> {
    inner: IndexSet<T, S>,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl<T: Eq + Hash, S: BuildHasher + Default> TrackedIndexSet<T, S> {
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
            inner: IndexSet::with_capacity_and_hasher(cap, S::default()),
            name,
            file,
            line,
            column,
        }
    }
}

impl<T: Eq + Hash, S: BuildHasher> TrackedIndexSet<T, S> {
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
            inner: IndexSet::with_capacity_and_hasher(cap, hasher),
            name,
            file,
            line,
            column,
        }
    }
}

impl<T, S> std::ops::Deref for TrackedIndexSet<T, S> {
    type Target = IndexSet<T, S>;
    fn deref(&self) -> &IndexSet<T, S> {
        &self.inner
    }
}

impl<T, S> std::ops::DerefMut for TrackedIndexSet<T, S> {
    fn deref_mut(&mut self) -> &mut IndexSet<T, S> {
        &mut self.inner
    }
}

impl<T, S> Drop for TrackedIndexSet<T, S> {
    fn drop(&mut self) {
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
    }
}

impl<T, S> From<TrackedIndexSet<T, S>> for IndexSet<T, S> {
    fn from(tracked: TrackedIndexSet<T, S>) -> IndexSet<T, S> {
        registry::record_sample(
            tracked.file,
            tracked.line,
            tracked.column,
            tracked.inner.capacity(),
        );
        // SAFETY: `tracked` is owned and forgotten below; ptr::read bit-copies
        // `inner` without requiring `Default` on S.
        let inner = unsafe { std::ptr::read(&tracked.inner) };
        std::mem::forget(tracked);
        inner
    }
}

impl<T, S> IntoInner for TrackedIndexSet<T, S> {
    type Inner = IndexSet<T, S>;
    #[inline]
    fn into_inner(self) -> IndexSet<T, S> {
        IndexSet::from(self)
    }
}

impl<T: Eq + Hash, S: BuildHasher> IntoIterator for TrackedIndexSet<T, S> {
    type Item = T;
    type IntoIter = indexmap::set::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
        // SAFETY: `self` is owned and forgotten below; ptr::read bit-copies `inner`.
        let inner = unsafe { std::ptr::read(&self.inner) };
        std::mem::forget(self);
        inner.into_iter()
    }
}
