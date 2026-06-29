// TrackedHashbrownMap telemetry tests — only compiled and run when `telemetry` is enabled.
// Verify that:
//   1. wrap_from records creation.
//   2. with_capacity_named records creation and capacity on Drop.
//   3. IntoIterator records sample before consuming inner.
//   4. CapInspect works on TrackedHashbrownMap.

use std::sync::atomic::Ordering;

use crate::registry;
use crate::{CapInspect, IntoInner, TrackedHashbrownMap};

fn peak(name: &'static str) -> usize {
    let mut m = 0;
    registry::registry().scan(|_, stats| {
        if stats.name == name {
            let samples = stats.samples.snapshot();
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
fn wrap_from_records_creation() {
    let before = count("test/hbmap/wrap_creation");
    let inner: hashbrown::HashMap<u32, u32> = hashbrown::HashMap::with_capacity(8);
    let _t = TrackedHashbrownMap::wrap_from(
        inner,
        "test/hbmap/wrap_creation",
        file!(),
        line!(),
        column!(),
    );
    let after = count("test/hbmap/wrap_creation");
    assert_eq!(after - before, 1, "wrap_from must increment creation_count");
}

#[test]
fn wrap_from_preserves_capacity() {
    let inner: hashbrown::HashMap<u32, u32> = hashbrown::HashMap::with_capacity(32);
    let cap = inner.capacity();
    let t =
        TrackedHashbrownMap::wrap_from(inner, "test/hbmap/wrap_cap", file!(), line!(), column!());
    assert!(t.capacity() >= cap, "wrap_from must not reduce capacity");
    drop(t);
    assert!(
        peak("test/hbmap/wrap_cap") >= cap,
        "drop must record sample >= initial capacity"
    );
}

#[test]
fn drop_records_capacity_sample() {
    {
        let _m = TrackedHashbrownMap::<u32, u32>::with_capacity_named(
            64,
            "test/hbmap/drop_cap",
            file!(),
            line!(),
            column!(),
        );
    }
    assert!(
        peak("test/hbmap/drop_cap") >= 64,
        "peak must be at least the initial capacity"
    );
}

#[test]
fn into_iter_records_sample_before_move() {
    {
        let m = TrackedHashbrownMap::<u32, u32>::with_capacity_named(
            16,
            "test/hbmap/into_iter",
            file!(),
            line!(),
            column!(),
        );
        let _collected: Vec<_> = m.into_iter().collect();
    }
    assert!(
        peak("test/hbmap/into_iter") >= 16,
        "peak must be recorded before inner is consumed by into_iter"
    );
}

#[test]
fn into_inner_converts_to_bare_hashbrown_map() {
    let t = TrackedHashbrownMap::<u32, u32>::with_capacity_named(
        48,
        "test/hbmap/into_inner",
        file!(),
        line!(),
        column!(),
    );
    let cap_before = t.capacity();
    let bare: hashbrown::HashMap<u32, u32> = t.into_inner();
    assert!(bare.capacity() >= cap_before);
    assert!(peak("test/hbmap/into_inner") >= cap_before);
}

#[test]
fn cap_inspect_works() {
    let t = TrackedHashbrownMap::<u32, u32>::with_capacity_named(
        128,
        "test/hbmap/cap_inspect",
        file!(),
        line!(),
        column!(),
    );
    // cap_inspect_at reads capacity and records it; this must not panic.
    t.cap_inspect_at("test/hbmap/cap_inspect_at", file!(), line!(), column!());
    assert!(t.capacity() >= 128);
}

#[test]
fn default_hasher_is_hashbrown_default() {
    // Verify the default S is hashbrown's DefaultHashBuilder by constructing
    // without explicit hasher annotation and checking the map is functional.
    let mut t = TrackedHashbrownMap::<u32, u32>::with_capacity_named(
        8,
        "test/hbmap/default_hasher",
        file!(),
        line!(),
        column!(),
    );
    t.insert(1, 10);
    t.insert(2, 20);
    assert_eq!(t.get(&1), Some(&10));
    assert_eq!(t.get(&2), Some(&20));
}
