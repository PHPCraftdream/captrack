// Tests for `CapInspect` — consumption-point transparent inspection (Phase L).
//
// These tests verify:
//   1. `cap_inspect_at` records a capacity sample (NOT a creation).
//   2. Peak-after-grow: calling cap_inspect after many pushes captures peak cap.
//   3. Smoke test: all 14 types compile with cap_inspect_at (telemetry-on).
//   4. Off-feature path: cap_inspect_at is a no-op (no panics, no registry hits).
//
// # Key constraint
//
// `file!()`, `line!()`, `column!()` expand to the source location of the macro
// call, so `record_creation` and `cap_inspect_at` called on different source
// lines produce different (file, line, col) keys.  To use the same key for
// both calls, we must capture the triple into variables first and reuse them.

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
                // Reservoir::snapshot() is non-destructive — no push-back needed.
                let samples = stats.samples.snapshot();
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

    /// Helper: count total_observed for entries matching `name`.
    fn total_observed_for(name: &str) -> u64 {
        let mut total = 0u64;
        registry::registry().scan(|_, stats| {
            if stats.name == name {
                total += stats.samples.total_observed();
            }
        });
        total
    }

    /// `cap_inspect_at` on a `Vec` records a capacity sample at the consumption
    /// point.  `creation_count` is NOT incremented by `cap_inspect_at` — the
    /// construction site must call `record_creation` / `record_initial` first.
    #[test]
    fn cap_inspect_records_at_consumption_point() {
        let name = "test:cap_inspect_records_at_consumption_point";
        // Capture the construction-site coordinates.  Both record_creation and
        // cap_inspect_at must use the same (file, line, col) key.
        let loc: (&'static str, u32, u32) = (file!(), line!(), column!());
        // Pre-register the construction site so the sample is not an orphan.
        // In real usage this is done by wrap_from / with_capacity_named.
        registry::record_creation(name, loc.0, loc.1, loc.2);
        let creation_after_ctor = creation_count_for(name);
        let observed_before = total_observed_for(name);

        let mut v: Vec<u32> = Vec::with_capacity(8);
        v.push(1);
        v.push(2);

        // Simulate a consumption point (e.g. return from a function).
        // cap_inspect_at should add a sample but NOT increment creation_count.
        CapInspect::cap_inspect_at(&v, name, loc.0, loc.1, loc.2);

        // creation_count unchanged by cap_inspect_at.
        assert_eq!(
            creation_count_for(name),
            creation_after_ctor,
            "cap_inspect_at must not increment creation_count"
        );
        // One more observation recorded.
        assert_eq!(
            total_observed_for(name),
            observed_before + 1,
            "cap_inspect_at must record exactly one sample"
        );
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
        // Capture location first, then register.
        let loc: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name, loc.0, loc.1, loc.2);

        // Start with a small cap, then push enough to trigger reallocations.
        let mut v: Vec<u32> = Vec::with_capacity(1);
        for i in 0..64u32 {
            v.push(i);
        }
        // At this point v.capacity() >= 64 (likely 64 or 128 depending on growth factor).
        let cap_at_inspect = v.capacity();
        assert!(cap_at_inspect >= 64, "Vec should have grown: cap={cap_at_inspect}");

        CapInspect::cap_inspect_at(&v, name, loc.0, loc.1, loc.2);

        let recorded = max_sample_for(name);
        assert_eq!(
            recorded,
            Some(cap_at_inspect),
            "should record post-grow capacity {cap_at_inspect}, got {recorded:?}"
        );
    }

    /// Smoke test: all 14 supported types call cap_inspect_at without panicking.
    ///
    /// Each type's call-site is pre-registered via `record_creation` so the
    /// sample is not an orphan.  `cap_inspect_at` must record samples but must
    /// NOT change creation_count.
    #[test]
    fn cap_inspect_works_for_each_type() {
        let name_prefix = "test:cap_inspect_works_for_each_type";

        // For each type we: capture loc, register the site, construct the type,
        // call cap_inspect_at with the same loc.  Because file!/line!/column!
        // expand to the macro invocation site we must interleave capture and use
        // carefully so the loc triple is the same for both calls.

        // Vec<u8>
        let name_vec = &*Box::leak(format!("{name_prefix}:vec").into_boxed_str());
        let loc_vec: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name_vec, loc_vec.0, loc_vec.1, loc_vec.2);
        let v: Vec<u8> = Vec::with_capacity(4);
        CapInspect::cap_inspect_at(&v, name_vec, loc_vec.0, loc_vec.1, loc_vec.2);

        // VecDeque<u8>
        let name_vd = &*Box::leak(format!("{name_prefix}:vecdeque").into_boxed_str());
        let loc_vd: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name_vd, loc_vd.0, loc_vd.1, loc_vd.2);
        let vd: std::collections::VecDeque<u8> = std::collections::VecDeque::with_capacity(4);
        CapInspect::cap_inspect_at(&vd, name_vd, loc_vd.0, loc_vd.1, loc_vd.2);

        // HashMap
        let name_hm = &*Box::leak(format!("{name_prefix}:hashmap").into_boxed_str());
        let loc_hm: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name_hm, loc_hm.0, loc_hm.1, loc_hm.2);
        let hm: std::collections::HashMap<u32, u32> = std::collections::HashMap::with_capacity(4);
        CapInspect::cap_inspect_at(&hm, name_hm, loc_hm.0, loc_hm.1, loc_hm.2);

        // HashSet
        let name_hs = &*Box::leak(format!("{name_prefix}:hashset").into_boxed_str());
        let loc_hs: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name_hs, loc_hs.0, loc_hs.1, loc_hs.2);
        let hs: std::collections::HashSet<u32> = std::collections::HashSet::with_capacity(4);
        CapInspect::cap_inspect_at(&hs, name_hs, loc_hs.0, loc_hs.1, loc_hs.2);

        // BTreeMap (uses len())
        let name_btm = &*Box::leak(format!("{name_prefix}:btreemap").into_boxed_str());
        let loc_btm: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name_btm, loc_btm.0, loc_btm.1, loc_btm.2);
        let btm: std::collections::BTreeMap<u32, u32> = std::collections::BTreeMap::new();
        CapInspect::cap_inspect_at(&btm, name_btm, loc_btm.0, loc_btm.1, loc_btm.2);

        // BTreeSet (uses len())
        let name_bts = &*Box::leak(format!("{name_prefix}:btreeset").into_boxed_str());
        let loc_bts: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name_bts, loc_bts.0, loc_bts.1, loc_bts.2);
        let bts: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        CapInspect::cap_inspect_at(&bts, name_bts, loc_bts.0, loc_bts.1, loc_bts.2);

        // BytesMut
        let name_bm = &*Box::leak(format!("{name_prefix}:bytesmut").into_boxed_str());
        let loc_bm: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name_bm, loc_bm.0, loc_bm.1, loc_bm.2);
        let bm = ::bytes::BytesMut::with_capacity(4);
        CapInspect::cap_inspect_at(&bm, name_bm, loc_bm.0, loc_bm.1, loc_bm.2);

        // IndexMap
        let name_im = &*Box::leak(format!("{name_prefix}:indexmap").into_boxed_str());
        let loc_im: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name_im, loc_im.0, loc_im.1, loc_im.2);
        let im: ::indexmap::IndexMap<u32, u32> = ::indexmap::IndexMap::with_capacity(4);
        CapInspect::cap_inspect_at(&im, name_im, loc_im.0, loc_im.1, loc_im.2);

        // IndexSet
        let name_is = &*Box::leak(format!("{name_prefix}:indexset").into_boxed_str());
        let loc_is: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name_is, loc_is.0, loc_is.1, loc_is.2);
        let is_: ::indexmap::IndexSet<u32> = ::indexmap::IndexSet::with_capacity(4);
        CapInspect::cap_inspect_at(&is_, name_is, loc_is.0, loc_is.1, loc_is.2);

        // DashMap
        let name_dm = &*Box::leak(format!("{name_prefix}:dashmap").into_boxed_str());
        let loc_dm: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name_dm, loc_dm.0, loc_dm.1, loc_dm.2);
        let dm: ::dashmap::DashMap<u32, u32> = ::dashmap::DashMap::with_capacity(4);
        CapInspect::cap_inspect_at(&dm, name_dm, loc_dm.0, loc_dm.1, loc_dm.2);

        // scc::HashMap
        let name_scchm = &*Box::leak(format!("{name_prefix}:scc_hashmap").into_boxed_str());
        let loc_scchm: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name_scchm, loc_scchm.0, loc_scchm.1, loc_scchm.2);
        let scchm: ::scc::HashMap<u32, u32> = ::scc::HashMap::with_capacity(4);
        CapInspect::cap_inspect_at(&scchm, name_scchm, loc_scchm.0, loc_scchm.1, loc_scchm.2);

        // scc::HashSet
        let name_scchs = &*Box::leak(format!("{name_prefix}:scc_hashset").into_boxed_str());
        let loc_scchs: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name_scchs, loc_scchs.0, loc_scchs.1, loc_scchs.2);
        let scchs: ::scc::HashSet<u32> = ::scc::HashSet::with_capacity(4);
        CapInspect::cap_inspect_at(&scchs, name_scchs, loc_scchs.0, loc_scchs.1, loc_scchs.2);

        // scc::TreeIndex (uses len())
        let name_tree = &*Box::leak(format!("{name_prefix}:scc_tree").into_boxed_str());
        let loc_tree: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name_tree, loc_tree.0, loc_tree.1, loc_tree.2);
        let tree: ::scc::TreeIndex<u32, u32> = ::scc::TreeIndex::new();
        CapInspect::cap_inspect_at(&tree, name_tree, loc_tree.0, loc_tree.1, loc_tree.2);

        // SmallVec
        let name_sv = &*Box::leak(format!("{name_prefix}:smallvec").into_boxed_str());
        let loc_sv: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name_sv, loc_sv.0, loc_sv.1, loc_sv.2);
        let sv: ::smallvec::SmallVec<[u8; 8]> = ::smallvec::SmallVec::with_capacity(8);
        CapInspect::cap_inspect_at(&sv, name_sv, loc_sv.0, loc_sv.1, loc_sv.2);

        // All 14 types fired — verify registry entries exist (created by record_creation).
        let mut found = false;
        registry::registry().scan(|_, stats| {
            if stats.name.starts_with(name_prefix) {
                found = true;
            }
        });
        assert!(found, "registry entries must exist (created via record_creation before cap_inspect_at)");
    }

    /// The block-expression form used by the lint injection compiles and works:
    /// `{ ::captrack::CapInspect::cap_inspect_at(&v, name, file!(), line!(), column!()); v }`
    /// The value `v` is returned by the block, type is preserved.
    ///
    /// `cap_inspect_at` adds a sample but does NOT increment creation_count.
    #[test]
    fn block_expression_form_preserves_type_and_value() {
        let name = "test:block_expression_form_preserves_type_and_value";
        // Capture the construction-site key, then register it.
        let loc: (&'static str, u32, u32) = (file!(), line!(), column!());
        registry::record_creation(name, loc.0, loc.1, loc.2);
        let creation_before = creation_count_for(name);

        let mut v: Vec<u32> = Vec::with_capacity(16);
        v.push(42);

        // This is the exact form the lint injects at consumption points.
        // Inside the crate we use `crate::CapInspect`; at call-sites in
        // user code the generated injection uses `::captrack::CapInspect`.
        let result: Vec<u32> = {
            crate::CapInspect::cap_inspect_at(&v, name, loc.0, loc.1, loc.2);
            v
        };

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], 42);
        assert!(result.capacity() >= 16);
        // creation_count must not have been incremented by cap_inspect_at.
        assert_eq!(
            creation_count_for(name),
            creation_before,
            "cap_inspect_at must not change creation_count; construction was registered via record_creation"
        );
    }
}

// Off-feature: CapInspect trait is available but all impls are no-ops.
// This test compiles in both modes; it verifies that calling cap_inspect_at
// in off-feature mode does NOT panic and does not produce observable side effects.
//
// In on-feature mode, cap_inspect_at calls record_sample which requires a
// prior record_creation for the same (file, line, col) key.  This test uses
// names that are NOT pre-registered, so in on-feature + debug mode the
// debug_assert in record_sample would fire.  To keep the test compilable in
// both modes without panicking, it is gated to off-feature only.
#[cfg(not(feature = "telemetry"))]
#[test]
fn cap_inspect_off_feature_is_noop_or_records_nothing_visible() {
    use crate::cap_inspect::CapInspect;

    // In off-feature mode: no-op — the call must complete without panic.
    let v: Vec<u32> = Vec::with_capacity(8);
    CapInspect::cap_inspect_at(&v, "test:noop_check", file!(), line!(), column!());

    // VecDeque
    let vd: std::collections::VecDeque<u32> = std::collections::VecDeque::with_capacity(4);
    CapInspect::cap_inspect_at(&vd, "test:noop_check_vd", file!(), line!(), column!());

    // No assertion — the point of the test is "does not panic".
}
