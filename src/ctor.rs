//! Free constructor functions — one per always-available collection family,
//! cfg-branched on `feature = "telemetry"`.
//!
//! Off-feature (`not(telemetry)`): thin wrappers with `#[inline(always)]`
//! so the compiler always folds them into bare `Vec::with_capacity(n)` etc.
//! The `let _: &'static str` lines are zero-cost type assertions.
//!
//! On-feature: delegate to `Tracked*::with_capacity_named(...)`, which
//! registers the creation in the global registry.
//!
//! These functions are called by the unified macros in `lib.rs`.
//!
//! # Scope
//!
//! Only the **always-available** std-based collection families are here:
//! `Vec`, `VecDeque`, `BTreeMap`, `BTreeSet`, `HashMap`, `HashSet`.
//!
//! Optional third-party families (`bytes`, `indexmap`, `dashmap`, `scc`)
//! cannot be placed here because their types aren't in the library's dep
//! graph unless the matching optional feature is active, while the macro
//! call-site resolution allows the consumer's external dep to supply the type.
//! Those macros keep dual `#[cfg]` arms in `lib.rs`.

// ─────────────────────────────────────────────────────────────────────────────
// tvec family
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(feature = "telemetry"))]
#[doc(hidden)]
#[inline(always)]
pub fn vec_with_capacity_named<T>(
    cap: usize,
    _name: &'static str,
    _file: &'static str,
    _line: u32,
    _column: u32,
) -> crate::TrackedVec<T> {
    #[allow(clippy::disallowed_methods)]
    Vec::with_capacity(cap)
}

#[cfg(feature = "telemetry")]
#[doc(hidden)]
#[inline(always)]
pub fn vec_with_capacity_named<T>(
    cap: usize,
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
) -> crate::TrackedVec<T> {
    crate::TrackedVec::with_capacity_named(cap, name, file, line, column)
}

// ─────────────────────────────────────────────────────────────────────────────
// tvecdeque family
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(feature = "telemetry"))]
#[doc(hidden)]
#[inline(always)]
pub fn vecdeque_with_capacity_named<T>(
    cap: usize,
    _name: &'static str,
    _file: &'static str,
    _line: u32,
    _column: u32,
) -> crate::TrackedVecDeque<T> {
    #[allow(clippy::disallowed_methods)]
    std::collections::VecDeque::with_capacity(cap)
}

#[cfg(feature = "telemetry")]
#[doc(hidden)]
#[inline(always)]
pub fn vecdeque_with_capacity_named<T>(
    cap: usize,
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
) -> crate::TrackedVecDeque<T> {
    crate::TrackedVecDeque::with_capacity_named(cap, name, file, line, column)
}

// ─────────────────────────────────────────────────────────────────────────────
// tbtreemap family  (cap hint ignored — BTreeMap has no with_capacity)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(feature = "telemetry"))]
#[doc(hidden)]
#[inline(always)]
pub fn btreemap_new_named<K: std::cmp::Ord, V>(
    _cap: usize,
    _name: &'static str,
    _file: &'static str,
    _line: u32,
    _column: u32,
) -> crate::TrackedBTreeMap<K, V> {
    #[allow(clippy::disallowed_methods)]
    std::collections::BTreeMap::new()
}

#[cfg(feature = "telemetry")]
#[doc(hidden)]
#[inline(always)]
pub fn btreemap_new_named<K: std::cmp::Ord, V>(
    cap: usize,
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
) -> crate::TrackedBTreeMap<K, V> {
    crate::TrackedBTreeMap::new_named(cap, name, file, line, column)
}

// ─────────────────────────────────────────────────────────────────────────────
// tbtreeset family  (cap hint ignored)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(feature = "telemetry"))]
#[doc(hidden)]
#[inline(always)]
pub fn btreeset_new_named<T: std::cmp::Ord>(
    _cap: usize,
    _name: &'static str,
    _file: &'static str,
    _line: u32,
    _column: u32,
) -> crate::TrackedBTreeSet<T> {
    #[allow(clippy::disallowed_methods)]
    std::collections::BTreeSet::new()
}

#[cfg(feature = "telemetry")]
#[doc(hidden)]
#[inline(always)]
pub fn btreeset_new_named<T: std::cmp::Ord>(
    cap: usize,
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
) -> crate::TrackedBTreeSet<T> {
    crate::TrackedBTreeSet::new_named(cap, name, file, line, column)
}

// ─────────────────────────────────────────────────────────────────────────────
// tfxmap family  (std::HashMap)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(feature = "telemetry"))]
#[doc(hidden)]
#[inline(always)]
pub fn hashmap_with_capacity_named<K, V, S: std::hash::BuildHasher + Default>(
    cap: usize,
    _name: &'static str,
    _file: &'static str,
    _line: u32,
    _column: u32,
) -> crate::TrackedHashMap<K, V, S> {
    #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
    std::collections::HashMap::with_capacity_and_hasher(cap, S::default())
}

#[cfg(feature = "telemetry")]
#[doc(hidden)]
#[inline(always)]
pub fn hashmap_with_capacity_named<
    K: Eq + std::hash::Hash,
    V,
    S: std::hash::BuildHasher + Default,
>(
    cap: usize,
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
) -> crate::TrackedHashMap<K, V, S> {
    crate::TrackedHashMap::with_capacity_named(cap, name, file, line, column)
}

#[cfg(not(feature = "telemetry"))]
#[doc(hidden)]
#[inline(always)]
pub fn hashmap_with_capacity_and_hasher_named<K, V, S: std::hash::BuildHasher>(
    cap: usize,
    hasher: S,
    _name: &'static str,
    _file: &'static str,
    _line: u32,
    _column: u32,
) -> crate::TrackedHashMap<K, V, S> {
    #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
    std::collections::HashMap::with_capacity_and_hasher(cap, hasher)
}

#[cfg(feature = "telemetry")]
#[doc(hidden)]
#[inline(always)]
pub fn hashmap_with_capacity_and_hasher_named<
    K: Eq + std::hash::Hash,
    V,
    S: std::hash::BuildHasher,
>(
    cap: usize,
    hasher: S,
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
) -> crate::TrackedHashMap<K, V, S> {
    crate::TrackedHashMap::with_capacity_and_hasher_named(cap, hasher, name, file, line, column)
}

// ─────────────────────────────────────────────────────────────────────────────
// tvec_owned family — returns bare Vec<T>, records initial cap only
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(feature = "telemetry"))]
#[doc(hidden)]
#[inline(always)]
pub fn vec_owned_with_capacity_named<T>(
    cap: usize,
    _name: &'static str,
    _file: &'static str,
    _line: u32,
    _column: u32,
) -> Vec<T> {
    #[allow(clippy::disallowed_methods)]
    Vec::with_capacity(cap)
}

#[cfg(feature = "telemetry")]
#[doc(hidden)]
#[inline(always)]
pub fn vec_owned_with_capacity_named<T>(
    cap: usize,
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
) -> Vec<T> {
    crate::registry::record_initial(name, file, line, column, cap);
    #[allow(clippy::disallowed_methods)]
    Vec::with_capacity(cap)
}

// ─────────────────────────────────────────────────────────────────────────────
// tvecdeque_owned family — returns bare VecDeque<T>, records initial cap only
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(feature = "telemetry"))]
#[doc(hidden)]
#[inline(always)]
pub fn vecdeque_owned_with_capacity_named<T>(
    cap: usize,
    _name: &'static str,
    _file: &'static str,
    _line: u32,
    _column: u32,
) -> std::collections::VecDeque<T> {
    #[allow(clippy::disallowed_methods)]
    std::collections::VecDeque::with_capacity(cap)
}

#[cfg(feature = "telemetry")]
#[doc(hidden)]
#[inline(always)]
pub fn vecdeque_owned_with_capacity_named<T>(
    cap: usize,
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
) -> std::collections::VecDeque<T> {
    crate::registry::record_initial(name, file, line, column, cap);
    #[allow(clippy::disallowed_methods)]
    std::collections::VecDeque::with_capacity(cap)
}

// ─────────────────────────────────────────────────────────────────────────────
// tfxmap_owned family — returns bare HashMap<K,V,S>, records initial cap only
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(feature = "telemetry"))]
#[doc(hidden)]
#[inline(always)]
pub fn hashmap_owned_with_capacity_named<K, V, S: std::hash::BuildHasher + Default>(
    cap: usize,
    _name: &'static str,
    _file: &'static str,
    _line: u32,
    _column: u32,
) -> std::collections::HashMap<K, V, S> {
    #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
    std::collections::HashMap::with_capacity_and_hasher(cap, S::default())
}

#[cfg(feature = "telemetry")]
#[doc(hidden)]
#[inline(always)]
pub fn hashmap_owned_with_capacity_named<K, V, S: std::hash::BuildHasher + Default>(
    cap: usize,
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
) -> std::collections::HashMap<K, V, S> {
    crate::registry::record_initial(name, file, line, column, cap);
    #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
    std::collections::HashMap::with_capacity_and_hasher(cap, S::default())
}

#[cfg(not(feature = "telemetry"))]
#[doc(hidden)]
#[inline(always)]
pub fn hashmap_owned_with_capacity_and_hasher_named<K, V, S: std::hash::BuildHasher>(
    cap: usize,
    hasher: S,
    _name: &'static str,
    _file: &'static str,
    _line: u32,
    _column: u32,
) -> std::collections::HashMap<K, V, S> {
    #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
    std::collections::HashMap::with_capacity_and_hasher(cap, hasher)
}

#[cfg(feature = "telemetry")]
#[doc(hidden)]
#[inline(always)]
pub fn hashmap_owned_with_capacity_and_hasher_named<K, V, S: std::hash::BuildHasher>(
    cap: usize,
    hasher: S,
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
) -> std::collections::HashMap<K, V, S> {
    crate::registry::record_initial(name, file, line, column, cap);
    #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
    std::collections::HashMap::with_capacity_and_hasher(cap, hasher)
}

// ─────────────────────────────────────────────────────────────────────────────
// tfxset_owned family — returns bare HashSet<T,S>, records initial cap only
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(feature = "telemetry"))]
#[doc(hidden)]
#[inline(always)]
pub fn hashset_owned_with_capacity_named<T, S: std::hash::BuildHasher + Default>(
    cap: usize,
    _name: &'static str,
    _file: &'static str,
    _line: u32,
    _column: u32,
) -> std::collections::HashSet<T, S> {
    #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
    std::collections::HashSet::with_capacity_and_hasher(cap, S::default())
}

#[cfg(feature = "telemetry")]
#[doc(hidden)]
#[inline(always)]
pub fn hashset_owned_with_capacity_named<T, S: std::hash::BuildHasher + Default>(
    cap: usize,
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
) -> std::collections::HashSet<T, S> {
    crate::registry::record_initial(name, file, line, column, cap);
    #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
    std::collections::HashSet::with_capacity_and_hasher(cap, S::default())
}

#[cfg(not(feature = "telemetry"))]
#[doc(hidden)]
#[inline(always)]
pub fn hashset_owned_with_capacity_and_hasher_named<T, S: std::hash::BuildHasher>(
    cap: usize,
    hasher: S,
    _name: &'static str,
    _file: &'static str,
    _line: u32,
    _column: u32,
) -> std::collections::HashSet<T, S> {
    #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
    std::collections::HashSet::with_capacity_and_hasher(cap, hasher)
}

#[cfg(feature = "telemetry")]
#[doc(hidden)]
#[inline(always)]
pub fn hashset_owned_with_capacity_and_hasher_named<T, S: std::hash::BuildHasher>(
    cap: usize,
    hasher: S,
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
) -> std::collections::HashSet<T, S> {
    crate::registry::record_initial(name, file, line, column, cap);
    #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
    std::collections::HashSet::with_capacity_and_hasher(cap, hasher)
}

// ─────────────────────────────────────────────────────────────────────────────
// tfxset family  (std::HashSet)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(feature = "telemetry"))]
#[doc(hidden)]
#[inline(always)]
pub fn hashset_with_capacity_named<T, S: std::hash::BuildHasher + Default>(
    cap: usize,
    _name: &'static str,
    _file: &'static str,
    _line: u32,
    _column: u32,
) -> crate::TrackedHashSet<T, S> {
    #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
    std::collections::HashSet::with_capacity_and_hasher(cap, S::default())
}

#[cfg(feature = "telemetry")]
#[doc(hidden)]
#[inline(always)]
pub fn hashset_with_capacity_named<T: Eq + std::hash::Hash, S: std::hash::BuildHasher + Default>(
    cap: usize,
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
) -> crate::TrackedHashSet<T, S> {
    crate::TrackedHashSet::with_capacity_named(cap, name, file, line, column)
}

#[cfg(not(feature = "telemetry"))]
#[doc(hidden)]
#[inline(always)]
pub fn hashset_with_capacity_and_hasher_named<T, S: std::hash::BuildHasher>(
    cap: usize,
    hasher: S,
    _name: &'static str,
    _file: &'static str,
    _line: u32,
    _column: u32,
) -> crate::TrackedHashSet<T, S> {
    #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
    std::collections::HashSet::with_capacity_and_hasher(cap, hasher)
}

#[cfg(feature = "telemetry")]
#[doc(hidden)]
#[inline(always)]
pub fn hashset_with_capacity_and_hasher_named<
    T: Eq + std::hash::Hash,
    S: std::hash::BuildHasher,
>(
    cap: usize,
    hasher: S,
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
) -> crate::TrackedHashSet<T, S> {
    crate::TrackedHashSet::with_capacity_and_hasher_named(cap, hasher, name, file, line, column)
}
