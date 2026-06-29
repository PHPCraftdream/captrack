// `CapInspect` — consumption-point transparent inspection.
//
// Phase L of captrack-pgo: instead of wrapping the construction of a
// collection (which requires changing the binding type from `X` to
// `TrackedX`), inject a side-effect read **at the point of consumption**
// (by-value escape: return, struct field init, fn arg, type-ascribed let).
//
// This is type-transparent: `cap_inspect_at` takes `&self` (shared ref),
// records the capacity, then the caller continues with the original
// un-wrapped value.  No E0308, no type change — the binding retains its
// original type throughout.

/// Capacity-inspect trait.  Provides a single method that reads the
/// current capacity / length of a collection and records it in the
/// captrack global registry (when `telemetry` is enabled) or is a
/// no-op (when `telemetry` is disabled).
///
/// # Usage
///
/// This trait is called automatically by the `captrack-pgo-lint` Phase L
/// injection — you should not need to call it manually.  The generated
/// code looks like:
///
/// ```rust,ignore
/// fn build() -> Vec<u8> {
///     let mut v = Vec::new();
///     fill(&mut v);
///     { ::captrack::CapInspect::cap_inspect_at(&v, "auto:...", file!(), line!(), column!()); v }
/// }
/// ```
///
/// # Feature gating
///
/// The `cap_inspect_at` method body (and `record_initial` call) are
/// compiled only when `feature = "telemetry"` is active.  In off-feature
/// mode all impls are no-ops inlined away by the compiler — zero overhead.
pub trait CapInspect {
    /// Record the capacity / length of `self` at the given call-site.
    ///
    /// The `name`, `file`, `line`, and `column` parameters mirror the
    /// arguments produced by `file!()`, `line!()`, `column!()` at the
    /// injection point.  `name` is the `"auto:<file>:<line>:<col>"` label
    /// that identifies the **construction** site of the binding (so the
    /// profile key matches the site where storage was allocated, not where
    /// it escaped).
    fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32);
}

// ─────────────────────────────────────────────────────────────────────────────
// Telemetry-on implementations (capacity- or len-based)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "telemetry")]
mod with_telemetry {
    use super::CapInspect;
    use crate::registry;

    // ── Vec<T> ───────────────────────────────────────────────────────────────

    impl<T> CapInspect for Vec<T> {
        #[inline]
        fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
            registry::record_initial(name, file, line, column, self.capacity());
        }
    }

    // ── VecDeque<T> ──────────────────────────────────────────────────────────

    impl<T> CapInspect for std::collections::VecDeque<T> {
        #[inline]
        fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
            registry::record_initial(name, file, line, column, self.capacity());
        }
    }

    // ── HashMap<K, V, S> ─────────────────────────────────────────────────────

    impl<K, V, S> CapInspect for std::collections::HashMap<K, V, S>
    where
        K: Eq + std::hash::Hash,
        S: std::hash::BuildHasher,
    {
        #[inline]
        fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
            registry::record_initial(name, file, line, column, self.capacity());
        }
    }

    // ── HashSet<T, S> ────────────────────────────────────────────────────────

    impl<T, S> CapInspect for std::collections::HashSet<T, S>
    where
        T: Eq + std::hash::Hash,
        S: std::hash::BuildHasher,
    {
        #[inline]
        fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
            registry::record_initial(name, file, line, column, self.capacity());
        }
    }

    // ── BTreeMap<K, V> — uses len() (no capacity()) ──────────────────────────

    impl<K: Ord, V> CapInspect for std::collections::BTreeMap<K, V> {
        #[inline]
        fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
            registry::record_initial(name, file, line, column, self.len());
        }
    }

    // ── BTreeSet<T> — uses len() (no capacity()) ─────────────────────────────

    impl<T: Ord> CapInspect for std::collections::BTreeSet<T> {
        #[inline]
        fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
            registry::record_initial(name, file, line, column, self.len());
        }
    }

    // ── bytes::BytesMut ──────────────────────────────────────────────────────

    impl CapInspect for ::bytes::BytesMut {
        #[inline]
        fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
            registry::record_initial(name, file, line, column, self.capacity());
        }
    }

    // ── indexmap::IndexMap<K, V, S> ──────────────────────────────────────────

    impl<K, V, S> CapInspect for ::indexmap::IndexMap<K, V, S>
    where
        K: Eq + std::hash::Hash,
        S: std::hash::BuildHasher,
    {
        #[inline]
        fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
            registry::record_initial(name, file, line, column, self.capacity());
        }
    }

    // ── indexmap::IndexSet<T, S> ─────────────────────────────────────────────

    impl<T, S> CapInspect for ::indexmap::IndexSet<T, S>
    where
        T: Eq + std::hash::Hash,
        S: std::hash::BuildHasher,
    {
        #[inline]
        fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
            registry::record_initial(name, file, line, column, self.capacity());
        }
    }

    // ── dashmap::DashMap<K, V, S> ────────────────────────────────────────────

    impl<K, V, S> CapInspect for ::dashmap::DashMap<K, V, S>
    where
        K: Eq + std::hash::Hash,
        S: std::hash::BuildHasher + Clone,
    {
        #[inline]
        fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
            registry::record_initial(name, file, line, column, self.capacity());
        }
    }

    // ── scc::HashMap<K, V, S> ────────────────────────────────────────────────

    impl<K, V, S> CapInspect for ::scc::HashMap<K, V, S>
    where
        K: Eq + std::hash::Hash + 'static,
        V: 'static,
        S: std::hash::BuildHasher,
    {
        #[inline]
        fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
            registry::record_initial(name, file, line, column, self.capacity());
        }
    }

    // ── scc::HashSet<T, S> ───────────────────────────────────────────────────

    impl<T, S> CapInspect for ::scc::HashSet<T, S>
    where
        T: Eq + std::hash::Hash + 'static,
        S: std::hash::BuildHasher,
    {
        #[inline]
        fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
            registry::record_initial(name, file, line, column, self.capacity());
        }
    }

    // ── scc::TreeIndex<K, V> — uses len() (no capacity()) ───────────────────

    impl<K, V> CapInspect for ::scc::TreeIndex<K, V>
    where
        K: Clone + Ord + 'static,
        V: Clone + 'static,
    {
        #[inline]
        fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
            // TreeIndex has no capacity(); len() is the best proxy.
            // NOTE: scc::TreeIndex::len() is O(N) — this is a profiling-only
            // code path, never on a hot production path. Acknowledged.
            #[allow(clippy::disallowed_methods)] // O(N) ack: telemetry-only path, never hot
            registry::record_initial(name, file, line, column, self.len());
        }
    }

    // ── smallvec::SmallVec<A> ────────────────────────────────────────────────

    impl<A: ::smallvec::Array> CapInspect for ::smallvec::SmallVec<A> {
        #[inline]
        fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
            registry::record_initial(name, file, line, column, self.capacity());
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Off-feature no-op implementations
//
// When `telemetry` is disabled the entire `cap_inspect_at` body is `{}` — a
// true no-op.  The compiler inlines and eliminates it completely.
// The signatures match those in `with_telemetry` so the generated usage-point
// injection compiles unchanged in both feature modes.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(feature = "telemetry"))]
mod without_telemetry {
    use super::CapInspect;

    impl<T> CapInspect for Vec<T> {
        #[inline(always)]
        fn cap_inspect_at(
            &self,
            _name: &'static str,
            _file: &'static str,
            _line: u32,
            _column: u32,
        ) {
        }
    }

    impl<T> CapInspect for std::collections::VecDeque<T> {
        #[inline(always)]
        fn cap_inspect_at(
            &self,
            _name: &'static str,
            _file: &'static str,
            _line: u32,
            _column: u32,
        ) {
        }
    }

    impl<K, V, S> CapInspect for std::collections::HashMap<K, V, S>
    where
        K: Eq + std::hash::Hash,
        S: std::hash::BuildHasher,
    {
        #[inline(always)]
        fn cap_inspect_at(
            &self,
            _name: &'static str,
            _file: &'static str,
            _line: u32,
            _column: u32,
        ) {
        }
    }

    impl<T, S> CapInspect for std::collections::HashSet<T, S>
    where
        T: Eq + std::hash::Hash,
        S: std::hash::BuildHasher,
    {
        #[inline(always)]
        fn cap_inspect_at(
            &self,
            _name: &'static str,
            _file: &'static str,
            _line: u32,
            _column: u32,
        ) {
        }
    }

    impl<K: Ord, V> CapInspect for std::collections::BTreeMap<K, V> {
        #[inline(always)]
        fn cap_inspect_at(
            &self,
            _name: &'static str,
            _file: &'static str,
            _line: u32,
            _column: u32,
        ) {
        }
    }

    impl<T: Ord> CapInspect for std::collections::BTreeSet<T> {
        #[inline(always)]
        fn cap_inspect_at(
            &self,
            _name: &'static str,
            _file: &'static str,
            _line: u32,
            _column: u32,
        ) {
        }
    }
}
