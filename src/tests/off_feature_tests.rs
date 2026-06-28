// Off-feature tests — verify that every macro expands to the correct bare type
// with the correct capacity.
//
// These tests are designed to pass in BOTH off-feature (default) and
// on-feature modes.  In off-feature mode the typed `let` bindings act as
// compile-time proofs that the macro returns the exact plain type.

#[allow(unused_imports)]
use crate::{
    tbtreemap, tbtreeset, tbytesmut, tdashmap, tfxmap, tfxset, tmap, tsccmap, tsccset, tscctree,
    tset, tvec, tvecdeque,
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
fn into_vec_identity_in_off_feature() {
    use crate::IntoVec;
    let mut v = tvec!("test/into_vec", 8);
    v.push(1u32);
    v.push(2u32);
    let raw: Vec<u32> = v.into_vec();
    assert_eq!(raw.len(), 2);
    assert!(raw.capacity() >= 8);
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
