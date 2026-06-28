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
                peak = peak.max(max_here);
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

// ── Регрессия: file!()/line!() ловят user call-site через слой macro_rules ──
//
// captrack ключует реестр по (file, line, column).  Это работает только если
// `file!()`/`line!()`/`column!()` внутри `tvec!` (и далее в свободной функции
// `ctor::vec_with_capacity_named`) резолвятся в источник КЛИЕНТА, а не в
// captrack/src/lib.rs (где они написаны).  Это стандартное поведение для
// std-макросов вроде `dbg!`/`log!`/`tracing!`, но через declare_collections!
// добавляется ВТОРОЙ слой macro_rules (`q_vec!` → `::captrack::tvec!` → ctor::)
// — span должен протащиться сквозь оба слоя.
//
// Если эти тесты упадут — это сигнал, что место создания пишется неправильно
// (например, у всех вызовов через q_*! в дампе будет видно captrack/src/lib.rs).

#[cfg(feature = "telemetry")]
fn lookup_loc(name: &'static str) -> Option<(&'static str, u32, u32)> {
    let mut found = None;
    captrack::registry::registry().scan(|key, stats| {
        if stats.name == name {
            found = Some(*key);
        }
    });
    found
}

#[cfg(feature = "telemetry")]
#[test]
fn span_captured_from_direct_tvec_call_site() {
    let expected_line = line!() + 1;
    let _v: captrack::TrackedVec<u32> = captrack::tvec!("loc/direct_tvec", 4);
    drop(_v);

    let (file, line, _column) = lookup_loc("loc/direct_tvec").expect("entry exists");
    assert!(
        file.replace('\\', "/")
            .ends_with("tests/declare_collections.rs"),
        "expected user call-site file, got {file}"
    );
    assert_eq!(line, expected_line, "line span lost");
}

#[cfg(feature = "telemetry")]
#[test]
fn span_captured_through_declare_collections_layer() {
    let expected_line = line!() + 1;
    let _v: captrack::TrackedVec<u32> = q_vec!("loc/via_declared", 4);
    drop(_v);

    let (file, line, _column) = lookup_loc("loc/via_declared").expect("entry exists");
    assert!(
        file.replace('\\', "/")
            .ends_with("tests/declare_collections.rs"),
        "span resolved to {file} — declare_collections! layer LOST user span"
    );
    assert_eq!(
        line, expected_line,
        "line resolved to {line} (expected {expected_line}) — declare_collections! layer LOST user span"
    );
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
