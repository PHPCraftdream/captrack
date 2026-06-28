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
    {
        let mut m = q_map!("declare/telemetry_map", 32);
        m.insert(1u32, "a");
    }
    let mut peak = 0usize;
    captrack::registry::registry().scan(|_, stats| {
        if stats.name == "declare/telemetry_map" {
            if let Ok(v) = stats.samples.lock() {
                if let Some(&max_here) = v.iter().max() {
                    peak = peak.max(max_here);
                }
            }
        }
    });
    assert!(
        peak >= 32,
        "telemetry must be recorded through declare_collections! macro (peak={peak})"
    );
}

// ── Задача 4: тесты для оставшихся 9 генерируемых имён ───────────────────────

#[test]
fn declare_collections_vecdeque_works() {
    let mut v = q_vecdeque!("declare/vecdeque", 8);
    v.push_back(1u32);
    v.push_back(2u32);
    assert_eq!(v.len(), 2);
}

#[test]
fn declare_collections_btreemap_works() {
    let mut m = q_btreemap!("declare/btreemap", 0);
    m.insert(1u32, "one");
    m.insert(2u32, "two");
    assert_eq!(m.len(), 2);
    assert_eq!(m.get(&1u32), Some(&"one"));
}

#[test]
fn declare_collections_btreeset_works() {
    let mut s = q_btreeset!("declare/btreeset", 0);
    s.insert(10u32);
    s.insert(20u32);
    assert_eq!(s.len(), 2);
    assert!(s.contains(&10u32));
}

#[test]
fn declare_collections_bytesmut_works() {
    let mut b = q_bytesmut!("declare/bytesmut", 64);
    assert!(b.capacity() >= 64);
    b.extend_from_slice(b"hello");
    assert_eq!(&b[..], b"hello");
}

#[test]
fn declare_collections_dashmap_works() {
    let d = q_dashmap!("declare/dashmap", 8);
    d.insert(1u32, "one");
    d.insert(2u32, "two");
    assert_eq!(d.len(), 2);
}

#[test]
fn declare_collections_sccmap_works() {
    let m = q_sccmap!("declare/sccmap", 8);
    let _ = m.insert(1u32, "one");
    let _ = m.insert(2u32, "two");
    #[allow(clippy::disallowed_methods)] // O(N) ack: test only
    let len = m.len();
    assert_eq!(len, 2);
}

#[test]
fn declare_collections_sccset_works() {
    let s = q_sccset!("declare/sccset", 8);
    let _ = s.insert(1u32);
    let _ = s.insert(2u32);
    #[allow(clippy::disallowed_methods)] // O(N) ack: test only
    let len = s.len();
    assert_eq!(len, 2);
}

#[test]
fn declare_collections_scctree_works() {
    let t = q_scctree!("declare/scctree", 0);
    let _ = t.insert(1u32, "one");
    let _ = t.insert(2u32, "two");
    #[allow(clippy::disallowed_methods)] // O(N) ack: test only
    let len = t.len();
    assert_eq!(len, 2);
}

#[test]
fn declare_collections_fxset_works() {
    let mut s = q_fxset!("declare/fxset", 4);
    s.insert(42u32);
    s.insert(100u32);
    assert_eq!(s.len(), 2);
    assert!(s.contains(&42u32));
}

// Тест на `;`-arm поверх кастомного дефолта.
#[test]
fn declare_collections_map_semicolon_arm_works() {
    // q_map! was generated with FxBuildHasher as default.
    // The `;`-arm lets callers still override per-call — even after declare_collections!.
    let mut m = q_map!("declare/override", 4; std::collections::hash_map::RandomState::new());
    m.insert(1u32, "one");
    m.insert(2u32, "two");
    assert_eq!(m.get(&1u32), Some(&"one"));
    assert_eq!(m.len(), 2);
}
