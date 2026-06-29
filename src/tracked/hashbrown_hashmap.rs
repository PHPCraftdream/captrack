// TrackedHashbrownMap wraps hashbrown::HashMap — generic over key, value, and build-hasher S.
// The default `S = hashbrown::DefaultHashBuilder` is the hashbrown crate default,
// which uses ahash internally.  Callers can override via `with_capacity_and_hasher_named`.

use hashbrown::DefaultHashBuilder;
use std::hash::{BuildHasher, Hash};

use crate::registry;
use crate::IntoInner;

/// A `hashbrown::HashMap<K, V, S>` wrapper that records creation count and peak capacity.
///
/// `S` defaults to `hashbrown::hash_map::DefaultHashBuilder` (ahash-backed), matching
/// the hashbrown crate default.  Use `with_capacity_and_hasher_named` to override.
pub struct TrackedHashbrownMap<K, V, S = DefaultHashBuilder> {
    inner: hashbrown::HashMap<K, V, S>,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl<K: Eq + Hash, V, S: BuildHasher + Default> TrackedHashbrownMap<K, V, S> {
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
            inner: hashbrown::HashMap::with_capacity_and_hasher(cap, S::default()),
            name,
            file,
            line,
            column,
        }
    }
}

impl<K: Eq + Hash, V, S: BuildHasher> TrackedHashbrownMap<K, V, S> {
    /// Create with an explicit hasher instance (per-call override).
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
            inner: hashbrown::HashMap::with_capacity_and_hasher(cap, hasher),
            name,
            file,
            line,
            column,
        }
    }

    /// Wrap an already-constructed `hashbrown::HashMap<K, V, S>` for capacity telemetry.
    ///
    /// Records creation in the registry; `inner` is moved as-is — no
    /// reallocation.  Capacity sample recorded at `Drop` as usual.
    #[inline]
    pub fn wrap_from(
        inner: hashbrown::HashMap<K, V, S>,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        registry::record_creation(name, file, line, column);
        Self { inner, name, file, line, column }
    }
}

impl<K, V, S> std::ops::Deref for TrackedHashbrownMap<K, V, S> {
    type Target = hashbrown::HashMap<K, V, S>;
    fn deref(&self) -> &hashbrown::HashMap<K, V, S> {
        &self.inner
    }
}

impl<K, V, S> std::ops::DerefMut for TrackedHashbrownMap<K, V, S> {
    fn deref_mut(&mut self) -> &mut hashbrown::HashMap<K, V, S> {
        &mut self.inner
    }
}

impl<K, V, S> Drop for TrackedHashbrownMap<K, V, S> {
    fn drop(&mut self) {
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
    }
}

impl<K, V, S> From<TrackedHashbrownMap<K, V, S>> for hashbrown::HashMap<K, V, S> {
    fn from(tracked: TrackedHashbrownMap<K, V, S>) -> hashbrown::HashMap<K, V, S> {
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

impl<K, V, S> IntoInner for TrackedHashbrownMap<K, V, S> {
    type Inner = hashbrown::HashMap<K, V, S>;
    #[inline]
    fn into_inner(self) -> hashbrown::HashMap<K, V, S> {
        hashbrown::HashMap::from(self)
    }
}

impl<K: Eq + Hash, V, S: BuildHasher> IntoIterator for TrackedHashbrownMap<K, V, S> {
    type Item = (K, V);
    type IntoIter = hashbrown::hash_map::IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
        // SAFETY: `self` is owned and forgotten below; ptr::read bit-copies `inner`.
        let inner = unsafe { std::ptr::read(&self.inner) };
        std::mem::forget(self);
        inner.into_iter()
    }
}

impl<K, V, S> crate::cap_inspect::CapInspect for TrackedHashbrownMap<K, V, S> {
    #[inline]
    fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
        registry::record_initial(name, file, line, column, self.inner.capacity());
    }
}
