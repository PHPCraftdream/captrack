// On-feature tests — only compiled and run when `telemetry` is enabled.
// Verify that:
//   1. peak_capacity is recorded in Drop (fetch_max semantics).
//   2. creation_count increments per instance.
//   3. Concurrent drops are safe and the peak survives.
//   4. dump_capacity_stats writes valid JSON with the expected structure.

use std::sync::atomic::Ordering;
use std::thread;

use crate::registry;
#[allow(unused_imports)]
use crate::{
    tbytesmut_owned, tdashmap_owned, tfxmap_owned, tfxset_owned, tmap_owned, tsccmap_owned,
    tsccset_owned, tset_owned, tvec_owned, tvecdeque_owned, IntoInner, TrackedBTreeMap,
    TrackedBTreeSet, TrackedBytesMut, TrackedDashMap, TrackedHashMap, TrackedHashSet,
    TrackedIndexMap, TrackedIndexSet, TrackedSccHashMap, TrackedSccHashSet, TrackedSccTreeIndex,
    TrackedVec, TrackedVecDeque,
};

// Helper: aggregate registry stats by name across all locations.
// Returns the maximum sample value seen (analogous to the old peak_capacity).
fn peak(name: &'static str) -> usize {
    let mut m = 0;
    registry::registry().scan(|_, stats| {
        if stats.name == name {
            // scc::Bag has no non-destructive shared-reference iterator; drain and
            // push back to keep the bag intact.
            let samples: Vec<usize> = stats.samples.pop_all(Vec::new(), |mut v, x| {
                v.push(x);
                v
            });
            for &s in &samples {
                stats.samples.push(s);
            }
            if let Some(&max_here) = samples.iter().max() {
                m = m.max(max_here);
            }
        }
    });
    m
}

fn count(name: &'static str) -> u64 {
    let mut s = 0;
    registry::registry().scan(|_, stats| {
        if stats.name == name {
            s += stats.creation_count.load(Ordering::Relaxed);
        }
    });
    s
}

#[test]
fn tvec_records_peak_on_drop() {
    {
        let mut v: TrackedVec<u32> = tvec!("on/peak_on_drop", 32);
        for i in 0..10u32 {
            v.push(i);
        }
        // Drop here — capacity should be >= 32 (only 10 items pushed, no realloc).
    }
    assert!(
        peak("on/peak_on_drop") >= 32,
        "peak must be at least the initial capacity"
    );
}

#[test]
fn tvec_records_creation_count() {
    let before = count("on/creation_count");
    for _ in 0..5 {
        let _v: TrackedVec<u8> = tvec!("on/creation_count", 4);
    }
    let after = count("on/creation_count");
    assert_eq!(after - before, 5, "creation_count must increment 5 times");
}

#[test]
fn peak_is_max_across_instances() {
    let before = peak("on/peak_is_max");
    {
        let _v1: TrackedVec<u8> = tvec!("on/peak_is_max", 10);
        let _v2: TrackedVec<u8> = tvec!("on/peak_is_max", 50);
        let _v3: TrackedVec<u8> = tvec!("on/peak_is_max", 20);
    }
    let after = peak("on/peak_is_max");
    assert!(
        after >= before.max(50),
        "peak must equal the largest capacity seen, got {after}"
    );
}

#[test]
fn concurrent_peak_record() {
    let handles: Vec<_> = (0usize..10)
        .map(|i| {
            thread::spawn(move || {
                for j in 0usize..20 {
                    let cap = (i * 20 + j + 1) * 4;
                    let _v: TrackedVec<u8> = tvec!("on/concurrent", cap);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread must not panic");
    }
    // Max capacity: thread 9, iter 19 → (9*20 + 19 + 1)*4 = 199 * 4 = 796.
    let p = peak("on/concurrent");
    assert!(
        p >= 796,
        "concurrent peak must capture the largest capacity: got {p}"
    );
}

#[test]
fn deref_works_like_vec() {
    let mut v: TrackedVec<u32> = tvec!("on/deref", 4);
    v.push(10u32);
    v.push(20);
    v.push(30);
    assert_eq!(v.len(), 3);
    assert_eq!(v[0], 10);
    assert_eq!(v[2], 30);
    let sum: u32 = v.iter().sum();
    assert_eq!(sum, 60);
}

#[test]
fn dump_writes_valid_json() {
    use std::io::Read;

    {
        let _v: TrackedVec<u8> = tvec!("on/dump", 128);
    }

    let dir = std::env::temp_dir().join("captrack_test");
    let path = dir.join("test_dump.json");
    crate::dump_capacity_stats(&path).expect("dump must succeed");

    let mut f = std::fs::File::open(&path).expect("dump file must exist");
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();

    let v: serde_json::Value = serde_json::from_str(&s).expect("must be valid JSON");
    assert_eq!(v["version"], 1, "JSON must have version=1");
    let stats = v["stats"].as_array().expect("stats must be an array");
    assert!(!stats.is_empty(), "stats array must not be empty");

    // Verify descending sort by max(samples).
    let max_samples: Vec<u64> = stats
        .iter()
        .map(|e| {
            e["samples"]
                .as_array()
                .and_then(|arr| arr.iter().filter_map(|v| v.as_u64()).max())
                .unwrap_or(0)
        })
        .collect();
    let mut sorted = max_samples.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(
        max_samples, sorted,
        "stats must be sorted by max(samples) descending"
    );

    let our_entry = stats
        .iter()
        .find(|e| e["name"].as_str() == Some("on/dump"))
        .expect("our named entry must appear in the dump");
    let our_samples = our_entry["samples"]
        .as_array()
        .expect("samples must be an array");
    assert!(
        !our_samples.is_empty(),
        "samples for on/dump must not be empty"
    );
    assert!(
        our_samples
            .iter()
            .filter_map(|v| v.as_u64())
            .max()
            .unwrap_or(0)
            >= 128,
        "max sample for on/dump must be >= 128"
    );
    assert!(
        our_entry["creation_count"].as_u64().unwrap_or(0) >= 1,
        "creation_count for on/dump must be >= 1"
    );
}

// ── Задача 1: тесты для оставшихся 12 wrapper'ов ─────────────────────────────

#[test]
fn tvecdeque_records_peak_and_count() {
    let before = count("on/vecdeque_pc");
    {
        let mut v: TrackedVecDeque<u32> = tvecdeque!("on/vecdeque_pc", 16);
        v.push_back(1u32);
    }
    assert!(peak("on/vecdeque_pc") >= 16);
    assert_eq!(count("on/vecdeque_pc") - before, 1);
}

#[test]
fn tbtreemap_records_peak_and_count() {
    // BTreeMap: Drop records len(), so insert N items and check peak >= N.
    let before = count("on/btreemap_pc");
    {
        let mut m: TrackedBTreeMap<u32, u32> = tbtreemap!("on/btreemap_pc", 0);
        m.insert(1, 10);
        m.insert(2, 20);
        m.insert(3, 30);
    }
    assert!(peak("on/btreemap_pc") >= 3);
    assert_eq!(count("on/btreemap_pc") - before, 1);
}

#[test]
fn tbtreeset_records_peak_and_count() {
    let before = count("on/btreeset_pc");
    {
        let mut s: TrackedBTreeSet<u32> = tbtreeset!("on/btreeset_pc", 0);
        s.insert(1);
        s.insert(2);
        s.insert(3);
        s.insert(4);
    }
    assert!(peak("on/btreeset_pc") >= 4);
    assert_eq!(count("on/btreeset_pc") - before, 1);
}

#[test]
fn tbytesmut_records_peak_and_count() {
    let before = count("on/bytesmut_pc");
    {
        let _b: TrackedBytesMut = tbytesmut!("on/bytesmut_pc", 64);
    }
    assert!(peak("on/bytesmut_pc") >= 64);
    assert_eq!(count("on/bytesmut_pc") - before, 1);
}

#[test]
fn tmap_records_peak_and_count() {
    let before = count("on/map_pc");
    {
        let mut m: TrackedIndexMap<u32, u32> = tmap!("on/map_pc", 8);
        m.insert(1, 2);
    }
    assert!(peak("on/map_pc") >= 8);
    assert_eq!(count("on/map_pc") - before, 1);
}

#[test]
fn tset_records_peak_and_count() {
    let before = count("on/set_pc");
    {
        let mut s: TrackedIndexSet<u32> = tset!("on/set_pc", 8);
        s.insert(42);
    }
    assert!(peak("on/set_pc") >= 8);
    assert_eq!(count("on/set_pc") - before, 1);
}

#[test]
fn tfxmap_records_peak_and_count() {
    let before = count("on/fxmap_pc");
    {
        let mut m: TrackedHashMap<u32, u32> = tfxmap!("on/fxmap_pc", 16);
        m.insert(1, 2);
    }
    assert!(peak("on/fxmap_pc") >= 16);
    assert_eq!(count("on/fxmap_pc") - before, 1);
}

#[test]
fn tfxset_records_peak_and_count() {
    let before = count("on/fxset_pc");
    {
        let mut s: TrackedHashSet<u32> = tfxset!("on/fxset_pc", 16);
        s.insert(1);
    }
    assert!(peak("on/fxset_pc") >= 16);
    assert_eq!(count("on/fxset_pc") - before, 1);
}

#[test]
fn tdashmap_records_peak_and_count() {
    // DashMap: Drop records len(), so insert N items and check peak >= N.
    let before = count("on/dashmap_pc");
    {
        let m: TrackedDashMap<u32, u32> = tdashmap!("on/dashmap_pc", 0);
        m.insert(1, 10);
        m.insert(2, 20);
        m.insert(3, 30);
    }
    assert!(peak("on/dashmap_pc") >= 3);
    assert_eq!(count("on/dashmap_pc") - before, 1);
}

#[test]
fn tsccmap_records_peak_and_count() {
    // scc::HashMap: Drop records len(), so insert N items and check peak >= N.
    let before = count("on/sccmap_pc");
    {
        let m: TrackedSccHashMap<u32, u32> = tsccmap!("on/sccmap_pc", 0);
        let _ = m.insert(1, 10);
        let _ = m.insert(2, 20);
        let _ = m.insert(3, 30);
    }
    assert!(peak("on/sccmap_pc") >= 3);
    assert_eq!(count("on/sccmap_pc") - before, 1);
}

#[test]
fn tsccset_records_peak_and_count() {
    // scc::HashSet: Drop records len(), so insert N items and check peak >= N.
    let before = count("on/sccset_pc");
    {
        let s: TrackedSccHashSet<u32> = tsccset!("on/sccset_pc", 0);
        let _ = s.insert(1);
        let _ = s.insert(2);
        let _ = s.insert(3);
    }
    assert!(peak("on/sccset_pc") >= 3);
    assert_eq!(count("on/sccset_pc") - before, 1);
}

#[test]
fn tscctree_records_peak_and_count() {
    // scc::TreeIndex: Drop records len(), so insert N items and check peak >= N.
    let before = count("on/scctree_pc");
    {
        let t: TrackedSccTreeIndex<u32, u32> = tscctree!("on/scctree_pc", 0);
        let _ = t.insert(1, 10);
        let _ = t.insert(2, 20);
        let _ = t.insert(3, 30);
    }
    assert!(peak("on/scctree_pc") >= 3);
    assert_eq!(count("on/scctree_pc") - before, 1);
}

// ── Задача 2: регрессионные тесты IntoIterator-ловушки ───────────────────────

#[test]
fn tvec_into_iter_records_peak_before_consume() {
    // Regression: into_iter must call record_peak BEFORE mem::take.
    // If the record_peak call is removed, Drop records 0 and this test fails.
    {
        let v: TrackedVec<u32> = tvec!("on/vec_iter", 32);
        let _collected: Vec<u32> = v.into_iter().collect();
    }
    assert!(
        peak("on/vec_iter") >= 32,
        "peak must be recorded before inner is consumed by into_iter"
    );
}

#[test]
fn tmap_into_iter_records_peak_before_consume() {
    // TrackedIndexMap also has IntoIterator with mem::replace + mem::forget.
    {
        let mut m: TrackedIndexMap<u32, u32> = tmap!("on/map_iter", 16);
        m.insert(1, 10);
        m.insert(2, 20);
        let _collected: Vec<(u32, u32)> = m.into_iter().collect();
    }
    assert!(
        peak("on/map_iter") >= 16,
        "peak must be recorded before inner is consumed by into_iter"
    );
}

#[test]
fn tfxmap_into_iter_records_peak_before_consume() {
    // TrackedHashMap has IntoIterator with mem::replace + mem::forget.
    {
        let mut m: TrackedHashMap<u32, u32> = tfxmap!("on/fxmap_iter", 16);
        m.insert(1, 10);
        m.insert(2, 20);
        let _collected: Vec<(u32, u32)> = m.into_iter().collect();
    }
    assert!(
        peak("on/fxmap_iter") >= 16,
        "peak must be recorded before inner is consumed by into_iter"
    );
}

#[test]
fn into_inner_records_sample_on_conversion() {
    let before = count("on/into_inner");
    {
        let mut v: TrackedVec<u32> = tvec!("on/into_inner", 64);
        v.push(1);
        let raw: Vec<u32> = v.into_inner();
        // raw is now a bare Vec — Drop runs on it, not on TrackedVec.
        assert_eq!(raw.len(), 1);
        assert!(raw.capacity() >= 64);
    }
    // Sample should be recorded by From<TrackedVec> for Vec (called via into_inner).
    // Without the explicit record_sample in From, peak would be 0.
    assert!(
        peak("on/into_inner") >= 64,
        "into_inner() must record capacity sample before unwrapping inner"
    );
    assert_eq!(count("on/into_inner") - before, 1);
}

#[test]
fn into_inner_supports_chained_method_inference() {
    // The C1 regression case — chain on a TrackedVec with NO post-conversion
    // type anchor.  `untrack!(v).len()` would fail on-feature with E0282
    // because `From::from` left the target type unconstrained.  `IntoInner`
    // has an associated `Inner` pinned by the source type, so `.len()` and
    // `.capacity()` resolve deterministically in both feature modes.
    let mut v = tvec!("on/chained", 8);
    v.push(1u32);
    let len = v.into_inner().len();
    assert_eq!(len, 1);

    let mut v2 = tvec!("on/chained2", 4);
    v2.push(7u32);
    let raw = v2.into_inner();
    assert_eq!(raw.capacity(), 4); // resolves only if raw: Vec<u32>
    let _: Vec<u32> = raw;

    // After both into_inner() conversions, samples were recorded for both
    // call-sites (capacity >= initial).
    assert!(peak("on/chained") >= 8);
    assert!(peak("on/chained2") >= 4);
}

#[test]
fn tbtreemap_into_iter_records_peak_before_consume() {
    // TrackedBTreeMap has IntoIterator with mem::take + mem::forget.
    // BTreeMap uses len() as peak, so we insert N items.
    {
        let mut m: TrackedBTreeMap<u32, u32> = tbtreemap!("on/btreemap_iter", 0);
        m.insert(1, 10);
        m.insert(2, 20);
        m.insert(3, 30);
        let _collected: Vec<(u32, u32)> = m.into_iter().collect();
    }
    assert!(
        peak("on/btreemap_iter") >= 3,
        "peak must be recorded (via len) before inner is consumed by into_iter"
    );
}

// ── N4: into_iter regressions for remaining 4 wrappers ───────────────────────

#[test]
fn tfxset_into_iter_records_peak_before_consume() {
    // TrackedHashSet uses capacity() as metric.
    // Regression: if record_sample is removed before ptr::read, Drop records 0.
    {
        let mut s: TrackedHashSet<u32> = tfxset!("on/fxset_iter", 32);
        s.insert(1);
        s.insert(2);
        let _collected: Vec<u32> = s.into_iter().collect();
    }
    assert!(
        peak("on/fxset_iter") >= 32,
        "peak must be recorded before inner is consumed by into_iter"
    );
}

#[test]
fn tset_into_iter_records_peak_before_consume() {
    // TrackedIndexSet uses capacity() as metric.
    {
        let mut s: TrackedIndexSet<u32> = tset!("on/set_iter", 16);
        s.insert(42);
        let _collected: Vec<u32> = s.into_iter().collect();
    }
    assert!(
        peak("on/set_iter") >= 16,
        "peak must be recorded before inner is consumed by into_iter"
    );
}

#[test]
fn tbtreeset_into_iter_records_peak_before_consume() {
    // TrackedBTreeSet uses len() as metric (no capacity concept).
    {
        let mut s: TrackedBTreeSet<u32> = tbtreeset!("on/btreeset_iter", 0);
        s.insert(1);
        s.insert(2);
        s.insert(3);
        let _collected: Vec<u32> = s.into_iter().collect();
    }
    assert!(
        peak("on/btreeset_iter") >= 3,
        "peak (len) must be recorded before inner is consumed by into_iter"
    );
}

#[test]
fn tvecdeque_into_iter_records_peak_before_consume() {
    // TrackedVecDeque uses capacity() as metric.
    {
        let mut d: TrackedVecDeque<u32> = tvecdeque!("on/vecdeque_iter", 32);
        d.push_back(1);
        d.push_back(2);
        let _collected: Vec<u32> = d.into_iter().collect();
    }
    assert!(
        peak("on/vecdeque_iter") >= 32,
        "peak must be recorded before inner is consumed by into_iter"
    );
}

// ── N5: into_inner tests for 12 non-Vec wrappers ─────────────────────────────

#[test]
fn into_inner_vecdeque_records_sample() {
    let mut d: TrackedVecDeque<u32> = tvecdeque!("on/into_inner_vecdeque", 32);
    d.push_back(1);
    let raw: std::collections::VecDeque<u32> = d.into_inner();
    assert!(raw.capacity() >= 32);
    assert!(peak("on/into_inner_vecdeque") >= 32);
}

#[test]
fn into_inner_btreemap_records_sample() {
    let mut m: TrackedBTreeMap<u32, u32> = tbtreemap!("on/into_inner_btreemap", 0);
    m.insert(1, 10);
    m.insert(2, 20);
    m.insert(3, 30);
    let raw: std::collections::BTreeMap<u32, u32> = m.into_inner();
    assert_eq!(raw.len(), 3);
    assert!(peak("on/into_inner_btreemap") >= 3);
}

#[test]
fn into_inner_btreeset_records_sample() {
    let mut s: TrackedBTreeSet<u32> = tbtreeset!("on/into_inner_btreeset", 0);
    s.insert(1);
    s.insert(2);
    s.insert(3);
    s.insert(4);
    let raw: std::collections::BTreeSet<u32> = s.into_inner();
    assert_eq!(raw.len(), 4);
    assert!(peak("on/into_inner_btreeset") >= 4);
}

#[test]
fn into_inner_bytesmut_records_sample() {
    let b: TrackedBytesMut = tbytesmut!("on/into_inner_bytesmut", 64);
    let raw: bytes::BytesMut = b.into_inner();
    assert!(raw.capacity() >= 64);
    assert!(peak("on/into_inner_bytesmut") >= 64);
}

#[test]
fn into_inner_hashmap_records_sample() {
    // I1 regression: Default bound on S was removed — conversion works without S: Default.
    let mut m: TrackedHashMap<u32, u32> = tfxmap!("on/into_inner_hashmap", 16);
    m.insert(1, 10);
    m.insert(2, 20);
    let raw: std::collections::HashMap<u32, u32, crate::CapHasher> = m.into_inner();
    assert!(raw.capacity() >= 16);
    assert!(peak("on/into_inner_hashmap") >= 16);
}

#[test]
fn into_inner_hashset_records_sample() {
    // I1 regression: Default bound on S was removed — conversion works without S: Default.
    let mut s: TrackedHashSet<u32> = tfxset!("on/into_inner_hashset", 16);
    s.insert(1);
    let raw: std::collections::HashSet<u32, crate::CapHasher> = s.into_inner();
    assert!(raw.capacity() >= 16);
    assert!(peak("on/into_inner_hashset") >= 16);
}

#[test]
fn into_inner_indexmap_records_sample() {
    // I1 regression: Default bound on S was removed.
    let mut m: TrackedIndexMap<u32, u32> = tmap!("on/into_inner_indexmap", 8);
    m.insert(1, 10);
    let raw: indexmap::IndexMap<u32, u32, crate::CapHasher> = m.into_inner();
    assert!(raw.capacity() >= 8);
    assert!(peak("on/into_inner_indexmap") >= 8);
}

#[test]
fn into_inner_indexset_records_sample() {
    // I1 regression: Default bound on S was removed.
    let mut s: TrackedIndexSet<u32> = tset!("on/into_inner_indexset", 8);
    s.insert(42);
    let raw: indexmap::IndexSet<u32, crate::CapHasher> = s.into_inner();
    assert!(raw.capacity() >= 8);
    assert!(peak("on/into_inner_indexset") >= 8);
}

#[test]
fn into_inner_dashmap_records_sample() {
    // I1 regression: Default bound on S was removed.
    let m: TrackedDashMap<u32, u32> = tdashmap!("on/into_inner_dashmap", 0);
    m.insert(1, 10);
    m.insert(2, 20);
    m.insert(3, 30);
    let raw: dashmap::DashMap<u32, u32, crate::CapHasher> = m.into_inner();
    // O(N) ack: test only — verifying the items are preserved after into_inner.
    let len = {
        #[allow(clippy::disallowed_methods)]
        raw.len()
    };
    assert_eq!(len, 3);
    assert!(peak("on/into_inner_dashmap") >= 3);
}

#[test]
fn into_inner_scc_hashmap_records_sample() {
    // I1 regression: Default bound on S was removed.
    let m: TrackedSccHashMap<u32, u32> = tsccmap!("on/into_inner_sccmap", 0);
    let _ = m.insert(1, 10);
    let _ = m.insert(2, 20);
    let _ = m.insert(3, 30);
    let raw: scc::HashMap<u32, u32, crate::CapHasher> = m.into_inner();
    // O(N) ack: test only.
    let len = {
        #[allow(clippy::disallowed_methods)]
        raw.len()
    };
    assert_eq!(len, 3);
    assert!(peak("on/into_inner_sccmap") >= 3);
}

#[test]
fn into_inner_scc_hashset_records_sample() {
    // I1 regression: Default bound on S was removed.
    let s: TrackedSccHashSet<u32> = tsccset!("on/into_inner_sccset", 0);
    let _ = s.insert(1);
    let _ = s.insert(2);
    let raw: scc::HashSet<u32, crate::CapHasher> = s.into_inner();
    // O(N) ack: test only.
    let len = {
        #[allow(clippy::disallowed_methods)]
        raw.len()
    };
    assert_eq!(len, 2);
    assert!(peak("on/into_inner_sccset") >= 2);
}

#[test]
fn into_inner_scc_treeindex_records_sample() {
    let t: TrackedSccTreeIndex<u32, u32> = tscctree!("on/into_inner_scctree", 0);
    let _ = t.insert(1, 10);
    let _ = t.insert(2, 20);
    let _ = t.insert(3, 30);
    let raw: scc::TreeIndex<u32, u32> = t.into_inner();
    // O(N) ack: test only.
    let len = {
        #[allow(clippy::disallowed_methods)]
        raw.len()
    };
    assert_eq!(len, 3);
    assert!(peak("on/into_inner_scctree") >= 3);
}

// ── t*_owned! on-feature tests — initial sample recorded ─────────────────────

#[test]
fn tvec_owned_records_initial_sample() {
    let before = count("on/owned/vec");
    let _v: Vec<u32> = tvec_owned!("on/owned/vec", 32);
    assert_eq!(count("on/owned/vec") - before, 1);
    assert!(peak("on/owned/vec") >= 32);
}

#[test]
fn tvecdeque_owned_records_initial_sample() {
    let before = count("on/owned/vecdeque");
    let _d: std::collections::VecDeque<u32> = tvecdeque_owned!("on/owned/vecdeque", 16);
    assert_eq!(count("on/owned/vecdeque") - before, 1);
    assert!(peak("on/owned/vecdeque") >= 16);
}

#[test]
fn tbytesmut_owned_records_initial_sample() {
    let before = count("on/owned/bytesmut");
    let _b = tbytesmut_owned!("on/owned/bytesmut", 64);
    assert_eq!(count("on/owned/bytesmut") - before, 1);
    assert!(peak("on/owned/bytesmut") >= 64);
}

#[test]
fn tfxmap_owned_records_initial_sample() {
    let before = count("on/owned/fxmap");
    let _m: std::collections::HashMap<u32, u32, crate::CapHasher> =
        tfxmap_owned!("on/owned/fxmap", 32);
    assert_eq!(count("on/owned/fxmap") - before, 1);
    assert!(peak("on/owned/fxmap") >= 32);
}

#[test]
fn tfxset_owned_records_initial_sample() {
    let before = count("on/owned/fxset");
    let _s: std::collections::HashSet<u32, crate::CapHasher> = tfxset_owned!("on/owned/fxset", 16);
    assert_eq!(count("on/owned/fxset") - before, 1);
    assert!(peak("on/owned/fxset") >= 16);
}

#[test]
fn tmap_owned_records_initial_sample() {
    let before = count("on/owned/imap");
    let _m: indexmap::IndexMap<u32, u32, crate::CapHasher> = tmap_owned!("on/owned/imap", 32);
    assert_eq!(count("on/owned/imap") - before, 1);
    assert!(peak("on/owned/imap") >= 32);
}

#[test]
fn tset_owned_records_initial_sample() {
    let before = count("on/owned/iset");
    let _s: indexmap::IndexSet<u32, crate::CapHasher> = tset_owned!("on/owned/iset", 16);
    assert_eq!(count("on/owned/iset") - before, 1);
    assert!(peak("on/owned/iset") >= 16);
}

#[test]
fn tdashmap_owned_records_initial_sample() {
    let before = count("on/owned/dashmap");
    let _d: dashmap::DashMap<u32, u32, crate::CapHasher> = tdashmap_owned!("on/owned/dashmap", 32);
    assert_eq!(count("on/owned/dashmap") - before, 1);
    assert!(peak("on/owned/dashmap") >= 32);
}

#[test]
fn tsccmap_owned_records_initial_sample() {
    let before = count("on/owned/sccmap");
    let _m: scc::HashMap<u32, u32, crate::CapHasher> = tsccmap_owned!("on/owned/sccmap", 32);
    assert_eq!(count("on/owned/sccmap") - before, 1);
    assert!(peak("on/owned/sccmap") >= 32);
}

#[test]
fn tsccset_owned_records_initial_sample() {
    let before = count("on/owned/sccset");
    let _s: scc::HashSet<u32, crate::CapHasher> = tsccset_owned!("on/owned/sccset", 16);
    assert_eq!(count("on/owned/sccset") - before, 1);
    assert!(peak("on/owned/sccset") >= 16);
}
