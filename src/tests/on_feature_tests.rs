// On-feature tests — only compiled and run when `telemetry` is enabled.
// Verify that:
//   1. peak_capacity is recorded in Drop (fetch_max semantics).
//   2. creation_count increments per instance.
//   3. Concurrent drops are safe and the peak survives.
//   4. dump_capacity_stats writes valid JSON with the expected structure.

use std::sync::atomic::Ordering;
use std::thread;

use crate::registry;
use crate::TrackedVec;

// Helper: read the registry stats for a given name.
fn peak(name: &'static str) -> usize {
    registry::registry()
        .get(&name)
        .map(|e| e.peak_capacity.load(Ordering::Relaxed))
        .unwrap_or(0)
}

fn count(name: &'static str) -> u64 {
    registry::registry()
        .get(&name)
        .map(|e| e.creation_count.load(Ordering::Relaxed))
        .unwrap_or(0)
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

    // Verify descending sort by peak_capacity.
    let peaks: Vec<u64> = stats
        .iter()
        .map(|e| e["peak_capacity"].as_u64().unwrap_or(0))
        .collect();
    let mut sorted = peaks.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(
        peaks, sorted,
        "stats must be sorted by peak_capacity descending"
    );

    let our_entry = stats
        .iter()
        .find(|e| e["name"].as_str() == Some("on/dump"))
        .expect("our named entry must appear in the dump");
    assert!(
        our_entry["peak_capacity"].as_u64().unwrap_or(0) >= 128,
        "peak for on/dump must be >= 128"
    );
    assert!(
        our_entry["creation_count"].as_u64().unwrap_or(0) >= 1,
        "creation_count for on/dump must be >= 1"
    );
}
