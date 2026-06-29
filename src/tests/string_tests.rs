// TrackedString telemetry tests — only compiled and run when `telemetry` is enabled.
// Verify that:
//   1. wrap_from records creation.
//   2. with_capacity_named records creation and capacity on Drop.
//   3. CapInspect works on TrackedString.

use std::sync::atomic::Ordering;

use crate::registry;
use crate::{CapInspect, IntoInner, TrackedString};

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
    let before = count("test/string/wrap_creation");
    let inner = String::with_capacity(32);
    let _t = TrackedString::wrap_from(inner, "test/string/wrap_creation", file!(), line!(), column!());
    let after = count("test/string/wrap_creation");
    assert_eq!(after - before, 1, "wrap_from must increment creation_count");
}

#[test]
fn wrap_from_preserves_capacity() {
    let inner = String::with_capacity(64);
    let cap = inner.capacity();
    let t = TrackedString::wrap_from(inner, "test/string/wrap_cap", file!(), line!(), column!());
    assert_eq!(t.capacity(), cap, "wrap_from must not reallocate");
    drop(t);
    assert!(
        peak("test/string/wrap_cap") >= cap,
        "drop must record sample >= initial capacity"
    );
}

#[test]
fn drop_records_capacity_sample() {
    {
        let s = TrackedString::with_capacity_named(128, "test/string/drop_cap", file!(), line!(), column!());
        drop(s);
    }
    assert!(
        peak("test/string/drop_cap") >= 128,
        "peak must be at least the initial capacity"
    );
}

#[test]
fn into_inner_converts_to_bare_string() {
    let t = TrackedString::with_capacity_named(16, "test/string/into_inner", file!(), line!(), column!());
    let bare: String = t.into_inner();
    assert!(bare.capacity() >= 16);
    assert!(peak("test/string/into_inner") >= 16);
}

#[test]
fn deref_allows_string_methods() {
    let mut t =
        TrackedString::with_capacity_named(64, "test/string/deref", file!(), line!(), column!());
    t.push_str("hello world");
    assert_eq!(t.len(), 11);
    assert!(t.contains("world"));
}

#[test]
fn cap_inspect_works() {
    let t =
        TrackedString::with_capacity_named(256, "test/string/cap_inspect", file!(), line!(), column!());
    // cap_inspect_at reads capacity and records it; this must not panic.
    t.cap_inspect_at("test/string/cap_inspect_at", file!(), line!(), column!());
    // The record is an initial-cap record (via record_initial).  The peak here is 256.
    assert!(t.capacity() >= 256);
}
