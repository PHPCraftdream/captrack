// TrackedBinaryHeap telemetry tests — only compiled and run when `telemetry` is enabled.
// Verify that:
//   1. wrap_from records creation.
//   2. with_capacity_named records creation and capacity on Drop.
//   3. IntoIterator records sample before consuming inner.
//   4. From impl preserves capacity.
//   5. CapInspect works.

use std::sync::atomic::Ordering;

use crate::registry;
use crate::{CapInspect, IntoInner, TrackedBinaryHeap};

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
    let before = count("test/bheap/wrap_creation");
    let inner: std::collections::BinaryHeap<u32> = std::collections::BinaryHeap::with_capacity(8);
    let _t = TrackedBinaryHeap::wrap_from(
        inner,
        "test/bheap/wrap_creation",
        file!(),
        line!(),
        column!(),
    );
    let after = count("test/bheap/wrap_creation");
    assert_eq!(after - before, 1, "wrap_from must increment creation_count");
}

#[test]
fn wrap_from_preserves_capacity() {
    let inner: std::collections::BinaryHeap<u32> =
        std::collections::BinaryHeap::with_capacity(32);
    let cap = inner.capacity();
    let t = TrackedBinaryHeap::wrap_from(inner, "test/bheap/wrap_cap", file!(), line!(), column!());
    assert_eq!(t.capacity(), cap, "wrap_from must not reallocate");
    drop(t);
    assert!(
        peak("test/bheap/wrap_cap") >= cap,
        "drop must record sample >= initial capacity"
    );
}

#[test]
fn drop_records_capacity_sample() {
    {
        let h = TrackedBinaryHeap::<u32>::with_capacity_named(
            64,
            "test/bheap/drop_cap",
            file!(),
            line!(),
            column!(),
        );
        drop(h);
    }
    assert!(
        peak("test/bheap/drop_cap") >= 64,
        "peak must be at least the initial capacity"
    );
}

#[test]
fn into_iter_records_sample_before_move() {
    {
        let h = TrackedBinaryHeap::<u32>::with_capacity_named(
            16,
            "test/bheap/into_iter",
            file!(),
            line!(),
            column!(),
        );
        let _collected: Vec<u32> = h.into_iter().collect();
    }
    assert!(
        peak("test/bheap/into_iter") >= 16,
        "peak must be recorded before inner is consumed by into_iter"
    );
}

#[test]
fn from_impl_preserves_inner() {
    let t = TrackedBinaryHeap::<u32>::with_capacity_named(
        32,
        "test/bheap/from_impl",
        file!(),
        line!(),
        column!(),
    );
    let cap_before = t.capacity();
    let bare: std::collections::BinaryHeap<u32> = t.into();
    assert_eq!(bare.capacity(), cap_before, "capacity must be preserved after From conversion");
    assert!(peak("test/bheap/from_impl") >= cap_before, "sample must be recorded by From impl");
}

#[test]
fn into_inner_is_consistent_with_from() {
    let t = TrackedBinaryHeap::<u32>::with_capacity_named(
        48,
        "test/bheap/into_inner",
        file!(),
        line!(),
        column!(),
    );
    let bare: std::collections::BinaryHeap<u32> = t.into_inner();
    assert!(bare.capacity() >= 48);
    assert!(peak("test/bheap/into_inner") >= 48);
}

#[test]
fn cap_inspect_works() {
    let t = TrackedBinaryHeap::<u32>::with_capacity_named(
        256,
        "test/bheap/cap_inspect",
        file!(),
        line!(),
        column!(),
    );
    t.cap_inspect_at("test/bheap/cap_inspect_at", file!(), line!(), column!());
    assert!(t.capacity() >= 256);
}

#[test]
fn heap_ordering_preserved() {
    let mut t = TrackedBinaryHeap::<u32>::with_capacity_named(
        8,
        "test/bheap/ordering",
        file!(),
        line!(),
        column!(),
    );
    t.push(5);
    t.push(1);
    t.push(10);
    t.push(3);
    // BinaryHeap is a max-heap — peek should give 10.
    assert_eq!(t.peek(), Some(&10));
    // Pop sequence must be in descending order.
    assert_eq!(t.pop(), Some(10));
    assert_eq!(t.pop(), Some(5));
}
