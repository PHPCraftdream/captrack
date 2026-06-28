// TrackedHashSet wraps std::HashSet — generic over the build-hasher S.

#![allow(clippy::disallowed_types)]

use std::collections::HashSet;
use std::hash::{BuildHasher, Hash};

use crate::registry;

/// A `HashSet<T, S>` wrapper that records creation count and peak capacity.
///
/// `S` defaults to `crate::CapHasher` (resolved by the active hasher feature).
pub struct TrackedHashSet<T, S = crate::CapHasher> {
    inner: HashSet<T, S>,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl<T: Eq + Hash, S: BuildHasher + Default> TrackedHashSet<T, S> {
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
            inner: HashSet::with_capacity_and_hasher(cap, S::default()),
            name,
            file,
            line,
            column,
        }
    }
}

impl<T: Eq + Hash, S: BuildHasher> TrackedHashSet<T, S> {
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
            inner: HashSet::with_capacity_and_hasher(cap, hasher),
            name,
            file,
            line,
            column,
        }
    }
}

impl<T, S> std::ops::Deref for TrackedHashSet<T, S> {
    type Target = HashSet<T, S>;
    fn deref(&self) -> &HashSet<T, S> {
        &self.inner
    }
}

impl<T, S> std::ops::DerefMut for TrackedHashSet<T, S> {
    fn deref_mut(&mut self) -> &mut HashSet<T, S> {
        &mut self.inner
    }
}

impl<T, S> Drop for TrackedHashSet<T, S> {
    fn drop(&mut self) {
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
    }
}

impl<T: Eq + Hash, S: BuildHasher + Default> IntoIterator for TrackedHashSet<T, S> {
    type Item = T;
    type IntoIter = std::collections::hash_set::IntoIter<T>;

    fn into_iter(mut self) -> Self::IntoIter {
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
        let inner = std::mem::replace(&mut self.inner, HashSet::with_hasher(S::default()));
        std::mem::forget(self);
        inner.into_iter()
    }
}
