// On-feature tests — only compiled and run when `telemetry` is enabled.
// Verify that:
//   1. peak_capacity is recorded in Drop (fetch_max semantics).
//   2. creation_count increments per instance.
//   3. Concurrent drops are safe and the peak survives.
//   4. dump_capacity_stats writes valid JSON with the expected structure.

use std::sync::atomic::Ordering;
use std::thread;

use crate::registry;
use crate::{
    TrackedBTreeMap, TrackedBTreeSet, TrackedBytesMut, TrackedDashMap, TrackedHashMap,
    TrackedHashSet, TrackedIndexMap, TrackedIndexSet, TrackedSccHashMap, TrackedSccHashSet,
    TrackedSccTreeIndex, TrackedVec, TrackedVecDeque,
};

// Helper: aggregate registry stats by name across all locations.
// Returns the maximum sample value seen (analogous to the old peak_capacity).
fn peak(name: &'static str) -> usize {
    let mut m = 0;
    registry::registry().scan(|_, stats| {
        if stats.name == name {
            if let Ok(v) = stats.samples.lock() {
                if let Some(&max_here) = v.iter().max() {
                    m = m.max(max_here);
                }
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
fn untrack_records_sample_on_conversion() {
    let before = count("on/untrack");
    {
        let mut v: TrackedVec<u32> = tvec!("on/untrack", 64);
        v.push(1);
        let raw: Vec<u32> = untrack!(v);
        // raw is now a bare Vec — Drop runs on it, not on TrackedVec.
        assert_eq!(raw.len(), 1);
        assert!(raw.capacity() >= 64);
    }
    // Sample should be recorded by From<TrackedVec> for Vec (called via untrack!).
    // Without the explicit record_sample in From, peak would be 0.
    assert!(
        peak("on/untrack") >= 64,
        "untrack!() must record capacity sample before unwrapping inner"
    );
    assert_eq!(count("on/untrack") - before, 1);
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
