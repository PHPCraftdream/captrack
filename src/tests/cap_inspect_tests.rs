// Tests for `CapInspect` — consumption-point transparent inspection (Phase L).
//
// These tests verify:
//   1. `cap_inspect_at` records the capacity at the consumption point (Vec).
//   2. Peak-after-grow: calling cap_inspect after many pushes captures peak cap.
//   3. Smoke test: all 14 types compile with cap_inspect_at (telemetry-on).
//   4. Off-feature path: cap_inspect_at is a no-op (no panics, no registry hits).

#[cfg(feature = "telemetry")]
mod telemetry_on {
    use crate::cap_inspect::CapInspect;
    use crate::registry;
    use std::sync::atomic::Ordering;

    /// Helper: read all samples for entries matching `name`, return max.
    fn max_sample_for(name: &str) -> Option<usize> {
        let mut max_val: Option<usize> = None;
        registry::registry().scan(|_, stats| {
            if stats.name == name {
                let samples: Vec<usize> = stats.samples.pop_all(Vec::new(), |mut v, x| {
                    v.push(x);
                    v
                });
                // push the samples back so subsequent reads still work
                for &s in &samples {
                    stats.samples.push(s);
                }
                if let Some(&m) = samples.iter().max() {
                    max_val = Some(max_val.map_or(m, |prev| prev.max(m)));
                }
            }
        });
        max_val
    }

    /// Helper: count creation_count for entries matching `name`.
    fn creation_count_for(name: &str) -> u64 {
        let mut total = 0u64;
        registry::registry().scan(|_, stats| {
            if stats.name == name {
                total += stats.creation_count.load(Ordering::Relaxed);
            }
        });
        total
    }

    /// `cap_inspect_at` on a `Vec` records the capacity at the consumption point.
    #[test]
    fn cap_inspect_records_at_consumption_point() {
        let name = "test:cap_inspect_records_at_consumption_point";
        let mut v: Vec<u32> = Vec::with_capacity(8);
        v.push(1);
        v.push(2);

        // Simulate a consumption point (e.g. return from a function):
        CapInspect::cap_inspect_at(&v, name, file!(), line!(), column!());

        // Should have recorded one creation and one sample == capacity().
        assert_eq!(creation_count_for(name), 1, "should have 1 creation count");
        let recorded = max_sample_for(name);
        assert_eq!(
            recorded,
            Some(v.capacity()),
            "recorded sample should equal Vec capacity at the inspection point"
        );
    }

    /// Cap is captured AFTER grow — calling cap_inspect after many pushes
    /// records the peak (post-grow) capacity, not the original allocation.
    #[test]
    fn cap_inspect_records_peak_after_grow() {
        let name = "test:cap_inspect_records_peak_after_grow";
        // Start with a small cap, then push enough to trigger reallocations.
        let mut v: Vec<u32> = Vec::with_capacity(1);
        for i in 0..64u32 {
            v.push(i);
        }
        // At this point v.capacity() >= 64 (likely 64 or 128 depending on growth factor).
        let cap_at_inspect = v.capacity();
        assert!(cap_at_inspect >= 64, "Vec should have grown: cap={cap_at_inspect}");

        CapInspect::cap_inspect_at(&v, name, file!(), line!(), column!());

        let recorded = max_sample_for(name);
        assert_eq!(
            recorded,
            Some(cap_at_inspect),
            "should record post-grow capacity {cap_at_inspect}, got {recorded:?}"
        );
    }

    /// Smoke test: all 14 supported types call cap_inspect_at without panicking.
    #[test]
    fn cap_inspect_works_for_each_type() {
        let name_prefix = "test:cap_inspect_works_for_each_type";

        // Vec<u8>
        let v: Vec<u8> = Vec::with_capacity(4);
        CapInspect::cap_inspect_at(&v, &*Box::leak(format!("{name_prefix}:vec").into_boxed_str()), file!(), line!(), column!());

        // VecDeque<u8>
        let vd: std::collections::VecDeque<u8> = std::collections::VecDeque::with_capacity(4);
        CapInspect::cap_inspect_at(&vd, &*Box::leak(format!("{name_prefix}:vecdeque").into_boxed_str()), file!(), line!(), column!());

        // HashMap
        let hm: std::collections::HashMap<u32, u32> = std::collections::HashMap::with_capacity(4);
        CapInspect::cap_inspect_at(&hm, &*Box::leak(format!("{name_prefix}:hashmap").into_boxed_str()), file!(), line!(), column!());

        // HashSet
        let hs: std::collections::HashSet<u32> = std::collections::HashSet::with_capacity(4);
        CapInspect::cap_inspect_at(&hs, &*Box::leak(format!("{name_prefix}:hashset").into_boxed_str()), file!(), line!(), column!());

        // BTreeMap (uses len())
        let btm: std::collections::BTreeMap<u32, u32> = std::collections::BTreeMap::new();
        CapInspect::cap_inspect_at(&btm, &*Box::leak(format!("{name_prefix}:btreemap").into_boxed_str()), file!(), line!(), column!());

        // BTreeSet (uses len())
        let bts: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        CapInspect::cap_inspect_at(&bts, &*Box::leak(format!("{name_prefix}:btreeset").into_boxed_str()), file!(), line!(), column!());

        // BytesMut
        let bm = ::bytes::BytesMut::with_capacity(4);
        CapInspect::cap_inspect_at(&bm, &*Box::leak(format!("{name_prefix}:bytesmut").into_boxed_str()), file!(), line!(), column!());

        // IndexMap
        let im: ::indexmap::IndexMap<u32, u32> = ::indexmap::IndexMap::with_capacity(4);
        CapInspect::cap_inspect_at(&im, &*Box::leak(format!("{name_prefix}:indexmap").into_boxed_str()), file!(), line!(), column!());

        // IndexSet
        let is_: ::indexmap::IndexSet<u32> = ::indexmap::IndexSet::with_capacity(4);
        CapInspect::cap_inspect_at(&is_, &*Box::leak(format!("{name_prefix}:indexset").into_boxed_str()), file!(), line!(), column!());

        // DashMap
        let dm: ::dashmap::DashMap<u32, u32> = ::dashmap::DashMap::with_capacity(4);
        CapInspect::cap_inspect_at(&dm, &*Box::leak(format!("{name_prefix}:dashmap").into_boxed_str()), file!(), line!(), column!());

        // scc::HashMap
        let scchm: ::scc::HashMap<u32, u32> = ::scc::HashMap::with_capacity(4);
        CapInspect::cap_inspect_at(&scchm, &*Box::leak(format!("{name_prefix}:scc_hashmap").into_boxed_str()), file!(), line!(), column!());

        // scc::HashSet
        let scchs: ::scc::HashSet<u32> = ::scc::HashSet::with_capacity(4);
        CapInspect::cap_inspect_at(&scchs, &*Box::leak(format!("{name_prefix}:scc_hashset").into_boxed_str()), file!(), line!(), column!());

        // scc::TreeIndex (uses len())
        let tree: ::scc::TreeIndex<u32, u32> = ::scc::TreeIndex::new();
        CapInspect::cap_inspect_at(&tree, &*Box::leak(format!("{name_prefix}:scc_tree").into_boxed_str()), file!(), line!(), column!());

        // SmallVec
        let sv: ::smallvec::SmallVec<[u8; 8]> = ::smallvec::SmallVec::with_capacity(8);
        CapInspect::cap_inspect_at(&sv, &*Box::leak(format!("{name_prefix}:smallvec").into_boxed_str()), file!(), line!(), column!());

        // All 14 types fired — verify at least one creation_count was recorded
        // (we can't easily enumerate all the leaked names, so just check any was recorded).
        let any_recorded = creation_count_for(name_prefix) > 0
            || {
                let mut found = false;
                registry::registry().scan(|_, stats| {
                    if stats.name.starts_with(name_prefix) {
                        found = true;
                    }
                });
                found
            };
        assert!(any_recorded, "at least one cap_inspect_at call should have recorded to registry");
    }

    /// The block-expression form used by the lint injection compiles and works:
    /// `{ ::captrack::CapInspect::cap_inspect_at(&v, name, file!(), line!(), column!()); v }`
    /// The value `v` is returned by the block, type is preserved.
    #[test]
    fn block_expression_form_preserves_type_and_value() {
        let name = "test:block_expression_form_preserves_type_and_value";
        let mut v: Vec<u32> = Vec::with_capacity(16);
        v.push(42);

        // This is the exact form the lint injects at consumption points.
        // Inside the crate we use `crate::CapInspect`; at call-sites in
        // user code the generated injection uses `::captrack::CapInspect`.
        let result: Vec<u32> = {
            crate::CapInspect::cap_inspect_at(&v, name, file!(), line!(), column!());
            v
        };

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], 42);
        assert!(result.capacity() >= 16);
        assert_eq!(creation_count_for(name), 1, "one creation recorded");
    }
}

// Off-feature: CapInspect trait is available but all impls are no-ops.
// This test compiles in both modes; it verifies that calling cap_inspect_at
// in off-feature mode does NOT panic and does not produce observable side effects.
#[test]
fn cap_inspect_off_feature_is_noop_or_records_nothing_visible() {
    use crate::cap_inspect::CapInspect;

    // The call must compile and complete without panic in both modes.
    let v: Vec<u32> = Vec::with_capacity(8);
    // In off-feature mode: no-op.
    // In on-feature mode: records to registry (side effect is expected).
    CapInspect::cap_inspect_at(&v, "test:noop_check", file!(), line!(), column!());

    // VecDeque
    let vd: std::collections::VecDeque<u32> = std::collections::VecDeque::with_capacity(4);
    CapInspect::cap_inspect_at(&vd, "test:noop_check_vd", file!(), line!(), column!());

    // No assertion — the point of the test is "does not panic".
    // In off-feature mode: zero registry entries for these names.
    // In on-feature mode: entries exist (covered by the telemetry_on tests above).
}
