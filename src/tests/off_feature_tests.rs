// Off-feature tests — verify that every macro expands to the correct bare type
// with the correct capacity.
//
// These tests are designed to pass in BOTH off-feature (default) and
// on-feature modes.  In off-feature mode the typed `let` bindings act as
// compile-time proofs that the macro returns the exact plain type.

#[allow(unused_imports)]
use crate::{
    tbtreemap, tbtreeset, tbytesmut, tbytesmut_owned, tdashmap, tdashmap_owned, tfxmap,
    tfxmap_owned, tfxset, tfxset_owned, tmap, tmap_owned, tsccmap, tsccmap_owned, tsccset,
    tsccset_owned, tscctree, tset, tset_owned, tvec, tvec_owned, tvecdeque, tvecdeque_owned,
    IntoInner,
};

#[test]
fn tvec_off_feature_is_plain_vec() {
    #[cfg(not(feature = "telemetry"))]
    {
        let v: Vec<u32> = tvec!("test/vec", 16);
        assert_eq!(v.capacity(), 16);
    }
    #[cfg(feature = "telemetry")]
    {
        let mut v = tvec!("test/vec", 16);
        v.push(1u32);
        assert_eq!(v.len(), 1);
    }
}

#[test]
fn tvecdeque_expands_with_capacity() {
    #[cfg(not(feature = "telemetry"))]
    {
        let v: std::collections::VecDeque<u32> = tvecdeque!("test/vecdeque", 8);
        assert_eq!(v.capacity(), 8);
    }
    #[cfg(feature = "telemetry")]
    {
        let mut v = tvecdeque!("test/vecdeque", 8);
        v.push_back(42u32);
        assert_eq!(v.len(), 1);
    }
}

#[test]
fn tbtreemap_expands_to_btreemap() {
    #[cfg(not(feature = "telemetry"))]
    {
        let m: std::collections::BTreeMap<u32, u32> = tbtreemap!("test/btreemap", 0);
        assert!(m.is_empty());
    }
    #[cfg(feature = "telemetry")]
    {
        let mut m = tbtreemap!("test/btreemap", 0);
        m.insert(1u32, 2u32);
        assert_eq!(m.len(), 1);
    }
}

#[test]
fn tbtreeset_expands_to_btreeset() {
    #[cfg(not(feature = "telemetry"))]
    {
        let s: std::collections::BTreeSet<u32> = tbtreeset!("test/btreeset", 0);
        assert!(s.is_empty());
    }
    #[cfg(feature = "telemetry")]
    {
        let mut s = tbtreeset!("test/btreeset", 0);
        s.insert(1u32);
        assert_eq!(s.len(), 1);
    }
}

#[test]
fn tfxmap_expands_with_hasher() {
    let mut m = tfxmap!("test/fxmap", 16);
    m.insert(1u32, 2u32);
    assert!(
        m.capacity() >= 16,
        "capacity must be at least the requested amount"
    );
    assert_eq!(m.len(), 1);
}

#[test]
fn tfxset_expands_with_hasher() {
    let mut s = tfxset!("test/fxset", 8);
    s.insert(1u32);
    assert!(
        s.capacity() >= 8,
        "capacity must be at least the requested amount"
    );
    assert_eq!(s.len(), 1);
}

#[test]
fn tmap_expands_to_indexmap() {
    let mut m = tmap!("test/map", 16);
    m.insert(1u32, 2u32);
    assert_eq!(m.capacity(), 16);
    assert_eq!(m.len(), 1);
}

#[test]
fn tset_expands_to_indexset() {
    let mut s = tset!("test/set", 8);
    s.insert(1u32);
    assert_eq!(s.capacity(), 8);
    assert_eq!(s.len(), 1);
}

#[test]
fn tdashmap_expands_to_dashmap() {
    let d = tdashmap!("test/dashmap", 16);
    d.insert(1u32, 2u32);
    assert_eq!(d.len(), 1);
}

#[test]
fn tsccmap_expands_to_scc_hashmap() {
    let m = tsccmap!("test/sccmap", 16);
    let _ = m.insert(1u32, 2u32);
    #[allow(clippy::disallowed_methods)] // O(N) ack: test only
    let len = m.len();
    assert_eq!(len, 1);
}

#[test]
fn tsccset_expands_to_scc_hashset() {
    let s = tsccset!("test/sccset", 8);
    let _ = s.insert(1u32);
    #[allow(clippy::disallowed_methods)] // O(N) ack: test only
    let len = s.len();
    assert_eq!(len, 1);
}

#[test]
fn tscctree_expands_to_scc_treeindex() {
    let t = tscctree!("test/scctree", 0);
    let _ = t.insert(1u32, 2u32);
    #[allow(clippy::disallowed_methods)] // O(N) ack: test only
    let len = t.len();
    assert_eq!(len, 1);
}

#[test]
fn tbytesmut_expands_with_capacity() {
    let mut b = tbytesmut!("test/bytesmut", 64);
    assert!(b.capacity() >= 64);
    b.extend_from_slice(b"hello");
    assert_eq!(&b[..], b"hello");
}

#[test]
fn dump_is_noop_in_off_feature() {
    #[cfg(not(feature = "telemetry"))]
    {
        let result = crate::dump_capacity_stats("this/path/is/never/opened.json");
        assert!(result.is_ok(), "dump must be no-op in off-feature");
    }
    #[cfg(feature = "telemetry")]
    {
        let dir = std::env::temp_dir().join("captrack_noop");
        let path = dir.join("noop.json");
        let result = crate::dump_capacity_stats(&path);
        assert!(result.is_ok());
    }
}

#[test]
fn into_inner_identity_in_off_feature() {
    let mut v = tvec!("test/into_inner", 8);
    v.push(1u32);
    v.push(2u32);
    let raw: Vec<u32> = v.into_inner();
    assert_eq!(raw.len(), 2);
    assert!(raw.capacity() >= 8);
}

#[test]
fn into_inner_supports_chained_method_inference() {
    // The C1 regression case — chain on a TrackedVec with NO post-conversion
    // type anchor.  `untrack!(v).len()` would fail on-feature with E0282
    // because `From::from` left the target type unconstrained.  `IntoInner`
    // has an associated `Inner` pinned by the source type, so `.len()` and
    // `.capacity()` resolve deterministically in both feature modes.
    let mut v = tvec!("test/chained", 8);
    v.push(1u32); // anchors T = u32
    let len = v.into_inner().len();
    assert_eq!(len, 1);

    // No type annotation on the receiving binding either — inferred purely
    // from `IntoInner::Inner = Vec<u32>` via the post-conversion `.capacity()`.
    let mut v2 = tvec!("test/chained2", 4);
    v2.push(7u32);
    let raw = v2.into_inner();
    assert_eq!(raw.capacity(), 4); // Vec::capacity resolves only if raw: Vec<u32>
    let _: Vec<u32> = raw; // post-hoc proof: inferred type IS Vec<u32>
}

// N1: type-equality proof for off-feature aliases.
// Verifies that TrackedX<T> IS the bare std/third-party type (zero-overhead
// alias contract). If someone accidentally wraps TrackedVec in a newtype
// off-feature, TypeId::of::<TrackedVec<u32>>() != TypeId::of::<Vec<u32>>() and
// this test fails.
#[cfg(not(feature = "telemetry"))]
#[test]
fn off_feature_aliases_are_type_equal_to_bare_types() {
    use std::any::TypeId;
    assert_eq!(
        TypeId::of::<crate::TrackedVec<u32>>(),
        TypeId::of::<Vec<u32>>()
    );
    assert_eq!(
        TypeId::of::<crate::TrackedVecDeque<u32>>(),
        TypeId::of::<std::collections::VecDeque<u32>>()
    );
    assert_eq!(
        TypeId::of::<crate::TrackedBTreeMap<u32, u32>>(),
        TypeId::of::<std::collections::BTreeMap<u32, u32>>()
    );
    assert_eq!(
        TypeId::of::<crate::TrackedBTreeSet<u32>>(),
        TypeId::of::<std::collections::BTreeSet<u32>>()
    );
    // HashMap/HashSet use CapHasher as default — compare against the same type expression.
    use std::collections::{HashMap, HashSet};
    assert_eq!(
        TypeId::of::<crate::TrackedHashMap<u32, u32>>(),
        TypeId::of::<HashMap<u32, u32, crate::CapHasher>>()
    );
    assert_eq!(
        TypeId::of::<crate::TrackedHashSet<u32>>(),
        TypeId::of::<HashSet<u32, crate::CapHasher>>()
    );
}

// N2: into_inner is a pure identity move off-feature, not a From conversion.
// Proves the move does not reallocate or copy: the pointer and capacity are
// preserved across the call boundary (i.e. no realloc, no From::from round-trip).
#[cfg(not(feature = "telemetry"))]
#[test]
fn off_feature_into_inner_is_pure_identity() {
    use crate::IntoInner;
    // Verify that this is a move, not a clone/conversion — via pointer identity.
    let v: Vec<u32> = tvec!("test/identity", 32);
    let ptr_before = v.as_ptr();
    let cap_before = v.capacity();
    let w = v.into_inner();
    // Move: pointer AND capacity are preserved (no realloc, no copy).
    assert_eq!(w.as_ptr(), ptr_before);
    assert_eq!(w.capacity(), cap_before);
}

/// Verify that CapHasher is RandomState when no hasher feature is active.
/// This is a type-inference test — the map must accept standard random keys.
#[cfg(not(any(
    feature = "fxhash",
    feature = "ahash",
    feature = "foldhash",
    feature = "rustc-hash"
)))]
#[test]
fn default_hasher_is_random_state() {
    // If CapHasher were NOT RandomState, this would fail to compile because
    // std::HashMap::with_capacity_and_hasher requires a BuildHasher.
    // We confirm the default expands to RandomState by type-annotating:
    use std::collections::hash_map::RandomState;
    // In off-feature mode tfxmap! expands to HashMap<K,V,CapHasher>.
    // We insert and retrieve to verify it works.
    let mut m = tfxmap!("test/random_state", 4);
    m.insert(String::from("key"), 42u32);
    assert_eq!(m.get("key"), Some(&42u32));
    // Also check: CapHasher IS RandomState (type alias test).
    let _: RandomState = <crate::CapHasher as Default>::default();
}

// ── t*_owned! tests — bare type returned in both feature modes ────────────────

#[test]
fn tvec_owned_returns_plain_vec() {
    let v: Vec<u32> = tvec_owned!("test/owned/vec", 8);
    assert_eq!(v.capacity(), 8);
}

#[test]
fn tvecdeque_owned_returns_plain_vecdeque() {
    let d: std::collections::VecDeque<u32> = tvecdeque_owned!("test/owned/vecdeque", 8);
    assert_eq!(d.capacity(), 8);
}

#[test]
fn tbytesmut_owned_returns_plain_bytesmut() {
    let b: bytes::BytesMut = tbytesmut_owned!("test/owned/bytesmut", 64);
    assert!(b.capacity() >= 64);
}

#[test]
fn tfxmap_owned_returns_plain_hashmap() {
    let m: std::collections::HashMap<u32, u32, crate::CapHasher> =
        tfxmap_owned!("test/owned/fxmap", 16);
    assert!(m.capacity() >= 16);
}

#[test]
fn tfxset_owned_returns_plain_hashset() {
    let s: std::collections::HashSet<u32, crate::CapHasher> = tfxset_owned!("test/owned/fxset", 8);
    assert!(s.capacity() >= 8);
}

#[test]
fn tmap_owned_returns_plain_indexmap() {
    let m: indexmap::IndexMap<u32, u32, crate::CapHasher> = tmap_owned!("test/owned/imap", 16);
    assert_eq!(m.capacity(), 16);
}

#[test]
fn tset_owned_returns_plain_indexset() {
    let s: indexmap::IndexSet<u32, crate::CapHasher> = tset_owned!("test/owned/iset", 8);
    assert_eq!(s.capacity(), 8);
}

#[test]
fn tdashmap_owned_returns_plain_dashmap() {
    let d: dashmap::DashMap<u32, u32, crate::CapHasher> = tdashmap_owned!("test/owned/dashmap", 16);
    d.insert(1u32, 2u32);
    assert_eq!(d.len(), 1);
}

#[test]
fn tsccmap_owned_returns_plain_scc_hashmap() {
    let m: scc::HashMap<u32, u32, crate::CapHasher> = tsccmap_owned!("test/owned/sccmap", 16);
    let _ = m.insert(1u32, 2u32);
    #[allow(clippy::disallowed_methods)]
    let len = m.len();
    assert_eq!(len, 1);
}

#[test]
fn tsccset_owned_returns_plain_scc_hashset() {
    let s: scc::HashSet<u32, crate::CapHasher> = tsccset_owned!("test/owned/sccset", 8);
    let _ = s.insert(1u32);
    #[allow(clippy::disallowed_methods)]
    let len = s.len();
    assert_eq!(len, 1);
}

#[test]
fn tsmallvec_expands_with_capacity() {
    #[cfg(not(feature = "telemetry"))]
    {
        let v: smallvec::SmallVec<[u8; 4]> = tsmallvec!("test/smallvec", 8);
        assert!(v.capacity() >= 8);
    }
    #[cfg(feature = "telemetry")]
    {
        let mut v: crate::TrackedSmallVec<[u8; 4]> = tsmallvec!("test/smallvec", 8);
        v.push(42u8);
        assert_eq!(v.len(), 1);
    }
}
