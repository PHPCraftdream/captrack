// Phase K: wrap_from tests — compiled only when `telemetry` is enabled.
//
// Verifies that `wrap_from` on each of the 14 TrackedType:
//   1. Records a creation entry in the registry.
//   2. Preserves the inner collection's capacity (or len for BTree/DashMap/scc* variants).
//   3. Records a sample at Drop.

use std::sync::atomic::Ordering;

use crate::registry;
use crate::{
    TrackedBTreeMap, TrackedBTreeSet, TrackedBytesMut, TrackedDashMap, TrackedHashMap,
    TrackedHashSet, TrackedIndexMap, TrackedIndexSet, TrackedSccHashMap, TrackedSccHashSet,
    TrackedSccTreeIndex, TrackedSmallVec, TrackedVec, TrackedVecDeque,
};

fn count(name: &'static str) -> u64 {
    let mut s = 0;
    registry::registry().scan(|_, stats| {
        if stats.name == name {
            s += stats.creation_count.load(Ordering::Relaxed);
        }
    });
    s
}

fn peek_max(name: &'static str) -> usize {
    let mut m = 0;
    registry::registry().scan(|_, stats| {
        if stats.name == name {
            // Reservoir::snapshot() is non-destructive — no push-back needed.
            let samples = stats.samples.snapshot();
            if let Some(&max_here) = samples.iter().max() {
                m = m.max(max_here);
            }
        }
    });
    m
}

// ── TrackedVec ───────────────────────────────────────────────────────────────

#[test]
fn wrap_from_vec_records_creation() {
    let before = count("wrap/vec/creation");
    let inner: Vec<u32> = vec![1, 2, 3];
    let _t = TrackedVec::wrap_from(inner, "wrap/vec/creation", file!(), line!(), column!());
    let after = count("wrap/vec/creation");
    assert_eq!(after - before, 1, "wrap_from must increment creation_count");
}

#[test]
fn wrap_from_vec_preserves_capacity() {
    let inner: Vec<u8> = Vec::with_capacity(64);
    let cap = inner.capacity();
    let t = TrackedVec::wrap_from(inner, "wrap/vec/cap", file!(), line!(), column!());
    assert_eq!(t.capacity(), cap, "wrap_from must not reallocate");
    drop(t);
    assert!(
        peek_max("wrap/vec/cap") >= cap,
        "drop must record sample >= initial capacity"
    );
}

#[test]
fn wrap_from_vec_list_literal() {
    // The key Phase K scenario: wrap a non-empty vec! without losing elements.
    let t = TrackedVec::wrap_from(
        vec![10u8, 20, 30],
        "wrap/vec/literal",
        file!(),
        line!(),
        column!(),
    );
    assert_eq!(t.len(), 3);
    assert_eq!(*t, [10u8, 20, 30]);
}

// ── TrackedVecDeque ──────────────────────────────────────────────────────────

#[test]
fn wrap_from_vecdeque_records_creation() {
    use std::collections::VecDeque;
    let before = count("wrap/vecdeque/creation");
    let inner: VecDeque<u32> = VecDeque::with_capacity(8);
    let _t = TrackedVecDeque::wrap_from(inner, "wrap/vecdeque/creation", file!(), line!(), column!());
    let after = count("wrap/vecdeque/creation");
    assert_eq!(after - before, 1);
}

#[test]
fn wrap_from_vecdeque_preserves_capacity() {
    use std::collections::VecDeque;
    let inner: VecDeque<u8> = VecDeque::with_capacity(32);
    let cap = inner.capacity();
    let t = TrackedVecDeque::wrap_from(inner, "wrap/vecdeque/cap", file!(), line!(), column!());
    assert!(t.capacity() >= cap);
    drop(t);
    assert!(peek_max("wrap/vecdeque/cap") >= cap);
}

// ── TrackedHashMap ───────────────────────────────────────────────────────────

#[test]
fn wrap_from_hashmap_records_creation() {
    use std::collections::HashMap;
    let before = count("wrap/hashmap/creation");
    let inner: HashMap<u32, u32> = HashMap::with_capacity(16);
    let _t = TrackedHashMap::wrap_from(inner, "wrap/hashmap/creation", file!(), line!(), column!());
    let after = count("wrap/hashmap/creation");
    assert_eq!(after - before, 1);
}

#[test]
fn wrap_from_hashmap_preserves_capacity() {
    use std::collections::HashMap;
    let inner: HashMap<u32, u32> = HashMap::with_capacity(32);
    let cap = inner.capacity();
    let t = TrackedHashMap::wrap_from(inner, "wrap/hashmap/cap", file!(), line!(), column!());
    assert!(t.capacity() >= cap);
    drop(t);
    assert!(peek_max("wrap/hashmap/cap") >= cap);
}

// ── TrackedHashSet ───────────────────────────────────────────────────────────

#[test]
fn wrap_from_hashset_records_creation() {
    use std::collections::HashSet;
    let before = count("wrap/hashset/creation");
    let inner: HashSet<u32> = HashSet::with_capacity(8);
    let _t = TrackedHashSet::wrap_from(inner, "wrap/hashset/creation", file!(), line!(), column!());
    let after = count("wrap/hashset/creation");
    assert_eq!(after - before, 1);
}

// ── TrackedBTreeMap ──────────────────────────────────────────────────────────

#[test]
fn wrap_from_btreemap_records_creation() {
    use std::collections::BTreeMap;
    let before = count("wrap/btreemap/creation");
    let mut inner: BTreeMap<u32, u32> = BTreeMap::new();
    inner.insert(1, 10);
    inner.insert(2, 20);
    let _t = TrackedBTreeMap::wrap_from(inner, "wrap/btreemap/creation", file!(), line!(), column!());
    let after = count("wrap/btreemap/creation");
    assert_eq!(after - before, 1);
}

#[test]
fn wrap_from_btreemap_preserves_elements() {
    use std::collections::BTreeMap;
    let mut inner: BTreeMap<u32, u32> = BTreeMap::new();
    inner.insert(5, 50);
    inner.insert(6, 60);
    let t = TrackedBTreeMap::wrap_from(inner, "wrap/btreemap/elems", file!(), line!(), column!());
    assert_eq!(t.len(), 2);
    assert_eq!(t.get(&5), Some(&50));
    drop(t);
    // BTreeMap sample = len at Drop
    assert!(peek_max("wrap/btreemap/elems") >= 2);
}

// ── TrackedBTreeSet ──────────────────────────────────────────────────────────

#[test]
fn wrap_from_btreeset_records_creation() {
    use std::collections::BTreeSet;
    let before = count("wrap/btreeset/creation");
    let inner: BTreeSet<u32> = [1u32, 2, 3].into_iter().collect();
    let _t = TrackedBTreeSet::wrap_from(inner, "wrap/btreeset/creation", file!(), line!(), column!());
    let after = count("wrap/btreeset/creation");
    assert_eq!(after - before, 1);
}

// ── TrackedBytesMut ──────────────────────────────────────────────────────────

#[test]
fn wrap_from_bytesmut_records_creation() {
    use bytes::BytesMut;
    let before = count("wrap/bytesmut/creation");
    let inner = BytesMut::with_capacity(128);
    let _t = TrackedBytesMut::wrap_from(inner, "wrap/bytesmut/creation", file!(), line!(), column!());
    let after = count("wrap/bytesmut/creation");
    assert_eq!(after - before, 1);
}

#[test]
fn wrap_from_bytesmut_preserves_capacity() {
    use bytes::BytesMut;
    let inner = BytesMut::with_capacity(256);
    let cap = inner.capacity();
    let t = TrackedBytesMut::wrap_from(inner, "wrap/bytesmut/cap", file!(), line!(), column!());
    assert!(t.capacity() >= cap);
    drop(t);
    assert!(peek_max("wrap/bytesmut/cap") >= cap);
}

// ── TrackedIndexMap ──────────────────────────────────────────────────────────

#[test]
fn wrap_from_indexmap_records_creation() {
    use indexmap::IndexMap;
    let before = count("wrap/indexmap/creation");
    let inner: IndexMap<u32, u32> = IndexMap::with_capacity(8);
    let _t = TrackedIndexMap::wrap_from(inner, "wrap/indexmap/creation", file!(), line!(), column!());
    let after = count("wrap/indexmap/creation");
    assert_eq!(after - before, 1);
}

// ── TrackedIndexSet ──────────────────────────────────────────────────────────

#[test]
fn wrap_from_indexset_records_creation() {
    use indexmap::IndexSet;
    let before = count("wrap/indexset/creation");
    let inner: IndexSet<u32> = IndexSet::with_capacity(4);
    let _t = TrackedIndexSet::wrap_from(inner, "wrap/indexset/creation", file!(), line!(), column!());
    let after = count("wrap/indexset/creation");
    assert_eq!(after - before, 1);
}

// ── TrackedDashMap ───────────────────────────────────────────────────────────

#[test]
fn wrap_from_dashmap_records_creation() {
    use dashmap::DashMap;
    let before = count("wrap/dashmap/creation");
    let inner: DashMap<u32, u32> = DashMap::new();
    let _t = TrackedDashMap::wrap_from(inner, "wrap/dashmap/creation", file!(), line!(), column!());
    let after = count("wrap/dashmap/creation");
    assert_eq!(after - before, 1);
}

// ── TrackedSccHashMap ────────────────────────────────────────────────────────

#[test]
fn wrap_from_scc_hashmap_records_creation() {
    let before = count("wrap/scc_hashmap/creation");
    let inner: scc::HashMap<u32, u32> = scc::HashMap::new();
    let _t = TrackedSccHashMap::wrap_from(inner, "wrap/scc_hashmap/creation", file!(), line!(), column!());
    let after = count("wrap/scc_hashmap/creation");
    assert_eq!(after - before, 1);
}

// ── TrackedSccHashSet ────────────────────────────────────────────────────────

#[test]
fn wrap_from_scc_hashset_records_creation() {
    let before = count("wrap/scc_hashset/creation");
    let inner: scc::HashSet<u32> = scc::HashSet::new();
    let _t = TrackedSccHashSet::wrap_from(inner, "wrap/scc_hashset/creation", file!(), line!(), column!());
    let after = count("wrap/scc_hashset/creation");
    assert_eq!(after - before, 1);
}

// ── TrackedSccTreeIndex ──────────────────────────────────────────────────────

#[test]
fn wrap_from_scc_treeindex_records_creation() {
    let before = count("wrap/scc_treeindex/creation");
    let inner: scc::TreeIndex<u32, u32> = scc::TreeIndex::new();
    let _t = TrackedSccTreeIndex::wrap_from(inner, "wrap/scc_treeindex/creation", file!(), line!(), column!());
    let after = count("wrap/scc_treeindex/creation");
    assert_eq!(after - before, 1);
}

// ── TrackedSmallVec ──────────────────────────────────────────────────────────

#[test]
fn wrap_from_smallvec_records_creation() {
    use smallvec::SmallVec;
    let before = count("wrap/smallvec/creation");
    let inner: SmallVec<[u8; 4]> = SmallVec::with_capacity(8);
    let _t = TrackedSmallVec::wrap_from(inner, "wrap/smallvec/creation", file!(), line!(), column!());
    let after = count("wrap/smallvec/creation");
    assert_eq!(after - before, 1);
}

#[test]
fn wrap_from_smallvec_preserves_elements() {
    use smallvec::{smallvec, SmallVec};
    // The key scenario: wrap a non-empty smallvec! without losing elements.
    let inner: SmallVec<[u32; 4]> = smallvec![1, 2, 3, 4, 5];
    let t = TrackedSmallVec::wrap_from(inner, "wrap/smallvec/elems", file!(), line!(), column!());
    assert_eq!(t.len(), 5);
    assert_eq!(&t[..], &[1u32, 2, 3, 4, 5]);
}

#[test]
fn wrap_from_smallvec_preserves_capacity() {
    use smallvec::SmallVec;
    let inner: SmallVec<[u8; 4]> = SmallVec::with_capacity(32);
    let cap = inner.capacity();
    let t = TrackedSmallVec::wrap_from(inner, "wrap/smallvec/cap", file!(), line!(), column!());
    assert!(t.capacity() >= cap);
    drop(t);
    assert!(peek_max("wrap/smallvec/cap") >= cap);
}
