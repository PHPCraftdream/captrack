// Integration test for `declare_collections!` (Axis 2C).
//
// This is an integration test (not a lib test) so `::captrack` resolves
// correctly — the generated macros call `::captrack::tmap!` etc. with
// absolute paths, which work from an external test binary.

captrack::declare_collections! { hasher = fxhash::FxBuildHasher, prefix = q }

#[test]
fn declare_collections_vec_works() {
    let mut v = q_vec!("declare/vec", 8);
    v.push(1u32);
    v.push(2u32);
    assert_eq!(v.len(), 2);
}

#[test]
fn declare_collections_map_uses_custom_hasher() {
    let mut m = q_map!("declare/map", 4);
    m.insert(10u32, "ten");
    m.insert(20u32, "twenty");
    assert_eq!(m.get(&10), Some(&"ten"));
    assert_eq!(m.get(&20), Some(&"twenty"));
}

#[test]
fn declare_collections_set_works() {
    let mut s = q_set!("declare/set", 4);
    s.insert(42u32);
    s.insert(100u32);
    assert_eq!(s.len(), 2);
    assert!(s.contains(&42u32));
}

#[test]
fn declare_collections_fxmap_also_works() {
    let mut m = q_fxmap!("declare/fxmap", 8);
    m.insert(1u64, "one");
    assert_eq!(m.get(&1u64), Some(&"one"));
}

#[cfg(feature = "telemetry")]
#[test]
fn declare_collections_map_telemetry_recorded() {
    use std::sync::atomic::Ordering;

    {
        let mut m = q_map!("declare/telemetry_map", 32);
        m.insert(1u32, "a");
    }
    let peak = captrack::registry::registry()
        .get(&"declare/telemetry_map")
        .map(|e| e.peak_capacity.load(Ordering::Relaxed))
        .unwrap_or(0);
    assert!(
        peak >= 32,
        "telemetry must be recorded through declare_collections! macro (peak={peak})"
    );
}
