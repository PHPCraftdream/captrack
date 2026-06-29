// SmallVec telemetry tests — only compiled and run when `telemetry` is enabled.
// Verify that:
//   1. Capacity is recorded in Drop.
//   2. creation_count increments on with_capacity_named.
//   3. into_iter records sample before the inner is consumed.
//   4. From impl preserves the inner SmallVec.

use std::sync::atomic::Ordering;

use crate::registry;
use crate::{IntoInner, TrackedSmallVec};
use smallvec::SmallVec;

// Helper: aggregate registry stats by name across all locations.
// Returns the maximum sample value seen.
fn peak(name: &'static str) -> usize {
    let mut m = 0;
    registry::registry().scan(|_, stats| {
        if stats.name == name {
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
fn tracked_smallvec_records_capacity_on_drop() {
    {
        let v: TrackedSmallVec<[u8; 4]> = tsmallvec!("test/smallvec/cap_drop", 8);
        drop(v);
    }
    assert!(
        peak("test/smallvec/cap_drop") >= 8,
        "peak must be at least the initial capacity"
    );
}

#[test]
fn tracked_smallvec_records_initial_capacity_via_with_capacity_named() {
    let before = count("test/smallvec/wc");
    {
        let _v =
            TrackedSmallVec::<[u8; 4]>::with_capacity_named(16, "test/smallvec/wc", file!(), line!(), column!());
    }
    let after = count("test/smallvec/wc");
    assert_eq!(after - before, 1, "creation_count must increment once");
}

#[test]
fn into_iter_records_sample_before_move() {
    {
        let v: TrackedSmallVec<[u8; 4]> = tsmallvec!("test/smallvec/iter", 4);
        let _collected: Vec<u8> = v.into_iter().collect();
    }
    assert!(
        peak("test/smallvec/iter") >= 4,
        "peak must be recorded before inner is consumed by into_iter"
    );
}

#[test]
fn from_impl_preserves_inner() {
    let t: TrackedSmallVec<[u8; 4]> = tsmallvec!("test/smallvec/from", 2);
    let cap_before = t.capacity();
    let bare: SmallVec<[u8; 4]> = t.into();
    assert_eq!(
        bare.capacity(),
        cap_before,
        "capacity must be preserved after From conversion"
    );
    assert!(
        peak("test/smallvec/from") >= 2,
        "sample must be recorded by From impl"
    );
}

#[test]
fn into_inner_is_consistent_with_from() {
    let t: TrackedSmallVec<[u8; 8]> = tsmallvec!("test/smallvec/into_inner", 16);
    let bare: SmallVec<[u8; 8]> = t.into_inner();
    assert!(bare.capacity() >= 16);
    assert!(peak("test/smallvec/into_inner") >= 16);
}
