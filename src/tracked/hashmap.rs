// TrackedHashMap wraps std::HashMap — generic over the build-hasher S.
// The default `S = crate::CapHasher` is set at the type level so that
// `tmap!("n", 16)` emits `TrackedHashMap::with_capacity_named(16, "n", ...)`
// and uses `CapHasher` without the caller needing to spell it out.

#![allow(clippy::disallowed_types)]

use std::collections::HashMap;
use std::hash::{BuildHasher, Hash};

use crate::registry;
use crate::IntoInner;

/// A `HashMap<K, V, S>` wrapper that records creation count and peak capacity.
///
/// `S` defaults to `crate::CapHasher` (resolved by the active hasher feature).
/// Use `with_capacity_and_hasher_named` to override the hasher per-call.
pub struct TrackedHashMap<K, V, S = crate::CapHasher> {
    inner: HashMap<K, V, S>,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl<K: Eq + Hash, V, S: BuildHasher + Default> TrackedHashMap<K, V, S> {
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
            inner: HashMap::with_capacity_and_hasher(cap, S::default()),
            name,
            file,
            line,
            column,
        }
    }
}

impl<K: Eq + Hash, V, S: BuildHasher> TrackedHashMap<K, V, S> {
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
            inner: HashMap::with_capacity_and_hasher(cap, hasher),
            name,
            file,
            line,
            column,
        }
    }
}

impl<K, V, S> std::ops::Deref for TrackedHashMap<K, V, S> {
    type Target = HashMap<K, V, S>;
    fn deref(&self) -> &HashMap<K, V, S> {
        &self.inner
    }
}

impl<K, V, S> std::ops::DerefMut for TrackedHashMap<K, V, S> {
    fn deref_mut(&mut self) -> &mut HashMap<K, V, S> {
        &mut self.inner
    }
}

impl<K, V, S> Drop for TrackedHashMap<K, V, S> {
    fn drop(&mut self) {
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
    }
}

impl<K, V, S> From<TrackedHashMap<K, V, S>> for HashMap<K, V, S> {
    fn from(tracked: TrackedHashMap<K, V, S>) -> HashMap<K, V, S> {
        registry::record_sample(
            tracked.file,
            tracked.line,
            tracked.column,
            tracked.inner.capacity(),
        );
        // SAFETY: `tracked` is owned by us and will be forgotten on the next
        // line, so its Drop never runs.  `ptr::read` bit-copies `inner` out;
        // ownership moves to the returned value.  Equivalent to
        // `mem::replace` but requires no `Default` bound on S.
        let inner = unsafe { std::ptr::read(&tracked.inner) };
        std::mem::forget(tracked);
        inner
    }
}

impl<K, V, S> IntoInner for TrackedHashMap<K, V, S> {
    type Inner = HashMap<K, V, S>;
    #[inline]
    fn into_inner(self) -> HashMap<K, V, S> {
        HashMap::from(self)
    }
}

impl<K: Eq + Hash, V, S: BuildHasher> IntoIterator for TrackedHashMap<K, V, S> {
    type Item = (K, V);
    type IntoIter = std::collections::hash_map::IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
        // SAFETY: `self` is owned and forgotten below; ptr::read bit-copies `inner`.
        let inner = unsafe { std::ptr::read(&self.inner) };
        std::mem::forget(self);
        inner.into_iter()
    }
}
