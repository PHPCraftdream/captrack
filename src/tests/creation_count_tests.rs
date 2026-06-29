// Tests verifying the `creation_count` / `total_observed` semantic invariants
// for mixed wrap_from + cap_inspect usage patterns.
//
// # Invariants under test
//
//   creation_count  = number of times the binding was **constructed**.
//                     Incremented by wrap_from / with_capacity_named / t*! macros.
//                     Never incremented by cap_inspect_at.
//
//   total_observed  = samples.total_observed() = total capacity observations.
//                     Incremented by every Drop / into_iter / cap_inspect_at call.
//
// For a mixed binding with 1 construction + N cap_inspects + 1 Drop:
//   creation_count == 1, total_observed >= N + 1.
//
// Orphan cap_inspect (no prior construction registration for the key):
//   the sample is silently discarded; no registry entry is created.

use std::sync::atomic::Ordering;

use crate::cap_inspect::CapInspect;
use crate::registry;
use crate::{TrackedVec, TrackedVecDeque};

// ── helpers ──────────────────────────────────────────────────────────────────

fn creation_count(name: &str) -> u64 {
    let mut total = 0u64;
    registry::registry().scan(|_, stats| {
        if stats.name == name {
            total += stats.creation_count.load(Ordering::Relaxed);
        }
    });
    total
}

fn total_observed(name: &str) -> u64 {
    let mut total = 0u64;
    registry::registry().scan(|_, stats| {
        if stats.name == name {
            total += stats.samples.total_observed();
        }
    });
    total
}

#[cfg(not(debug_assertions))]
fn has_entry(name: &str) -> bool {
    let mut found = false;
    registry::registry().scan(|_, stats| {
        if stats.name == name {
            found = true;
        }
    });
    found
}

// ── test 1: safe-only binding ─────────────────────────────────────────────────

/// A safe-only binding: wrap_from → safe usages (push / len / &v) → Drop.
///
/// Expected: creation_count == 1, total_observed == 1 (Drop sample only).
/// cap_inspect_at is never called in this pattern.
#[test]
fn safe_only_binding_creation_count_equals_one() {
    let name = "creation_count_tests:safe_only";
    let before_cc = creation_count(name);
    let before_to = total_observed(name);
    {
        let mut v = TrackedVec::wrap_from(
            Vec::<u32>::with_capacity(32),
            name,
            file!(),
            line!(),
            column!(),
        );
        // Safe usages only — no cap_inspect_at.
        v.push(1u32);
        v.push(2);
        let _len = v.len();
        let _ref: &[u32] = &v;
    } // Drop here — records one sample.

    assert_eq!(
        creation_count(name) - before_cc,
        1,
        "wrap_from must increment creation_count exactly once"
    );
    assert_eq!(
        total_observed(name) - before_to,
        1,
        "safe-only binding: exactly one Drop sample"
    );
}

// ── test 2: mixed binding — creation_count stays 1 ───────────────────────────

/// A mixed binding: 1 wrap_from + 3 simulated cap_inspect (unsafe/escape usages) + Drop.
///
/// Expected: creation_count == 1, total_observed >= 4.
///
/// In real Phase L injection, cap_inspect_at is passed the construction site
/// coordinates.  We replicate this by registering via record_creation first,
/// then calling cap_inspect_at with the same (file, line, column) key.
#[test]
fn mixed_binding_creation_count_stays_one() {
    let name = "creation_count_tests:mixed";
    // Simulate a construction site at a fixed (file, line, column) triple.
    // record_creation registers the entry; wrap_from in real code does this.
    let (ctor_file, ctor_line, ctor_col) = (file!(), line!(), column!());
    registry::record_creation(name, ctor_file, ctor_line, ctor_col);
    let before_cc = creation_count(name);
    let before_to = total_observed(name);

    let v: Vec<u32> = Vec::with_capacity(16);

    // Simulate 3 cap_inspect calls (consumption points: return, struct field,
    // fn arg) using the construction site key.
    CapInspect::cap_inspect_at(&v, name, ctor_file, ctor_line, ctor_col);
    CapInspect::cap_inspect_at(&v, name, ctor_file, ctor_line, ctor_col);
    CapInspect::cap_inspect_at(&v, name, ctor_file, ctor_line, ctor_col);

    // No additional creation — cap_inspect_at must not touch creation_count.
    assert_eq!(
        creation_count(name) - before_cc,
        0,
        "cap_inspect_at must not increment creation_count; it was already set by record_creation"
    );
    assert_eq!(
        total_observed(name) - before_to,
        3,
        "3 cap_inspect_at calls must produce 3 samples"
    );

    // Now simulate Drop (record_sample).
    registry::record_sample(ctor_file, ctor_line, ctor_col, v.capacity());

    assert_eq!(
        total_observed(name) - before_to,
        4,
        "3 cap_inspect + 1 Drop = 4 total observations"
    );
    // creation_count still 0 extra (still 1 total from the initial record_creation).
    assert_eq!(
        creation_count(name) - before_cc,
        0,
        "creation_count must remain unchanged after all samples"
    );
}

// ── test 3: orphan cap_inspect (no prior registration) ───────────────────────

/// cap_inspect_at for a location that was never registered by a construction
/// site call is a safe no-op in **release** mode: no registry entry is
/// created, no panic.  In **debug** mode a `debug_assert` fires — this is
/// intentional (early signal for a misconfigured instrument pass).
///
/// This test exercises the release-mode path only (no debug_assertions).
/// The debug-mode assertion behaviour is documented in `registry::record_sample`.
#[test]
#[cfg(not(debug_assertions))]
fn cap_inspect_only_site_does_not_create_orphan_entry() {
    // Use a unique name that was never registered.
    let orphan_name = "creation_count_tests:orphan_never_registered";
    // Choose a (file, line, col) that definitely was not registered.
    let orphan_file = "orphan_src/not_a_real_file.rs";
    let orphan_line: u32 = 99_999;
    let orphan_col: u32 = 1;

    assert!(
        !has_entry(orphan_name),
        "precondition: orphan site must not be in the registry"
    );

    let v: Vec<u32> = Vec::with_capacity(8);
    // In release mode this is a silent no-op: record_sample sees no entry and
    // returns without recording.
    CapInspect::cap_inspect_at(&v, orphan_name, orphan_file, orphan_line, orphan_col);

    // No entry must have been created.
    assert!(
        !has_entry(orphan_name),
        "cap_inspect_at on unregistered site must not create a registry entry"
    );
    assert_eq!(
        creation_count(orphan_name),
        0,
        "orphan cap_inspect must not affect creation_count"
    );
    assert_eq!(
        total_observed(orphan_name),
        0,
        "orphan cap_inspect must not record any observations"
    );
}

// ── test 4: mixed — samples appear in reservoir ───────────────────────────────

/// Multiple cap_inspect calls for the same site accumulate samples in the
/// reservoir and all count toward total_observed.
#[test]
fn mixed_consumption_samples_in_reservoir() {
    let name = "creation_count_tests:reservoir_samples";
    let (ctor_file, ctor_line, ctor_col) = (file!(), line!(), column!());
    registry::record_creation(name, ctor_file, ctor_line, ctor_col);
    let before_to = total_observed(name);

    // Inspect with varying capacities.
    let caps: &[usize] = &[8, 16, 32, 64];
    for &cap in caps {
        let v: Vec<u8> = Vec::with_capacity(cap);
        CapInspect::cap_inspect_at(&v, name, ctor_file, ctor_line, ctor_col);
    }

    assert_eq!(
        total_observed(name) - before_to,
        caps.len() as u64,
        "total_observed must equal the number of cap_inspect_at calls"
    );

    // All samples should appear in the reservoir snapshot.
    let mut snapshot_max = 0usize;
    registry::registry().scan(|_, stats| {
        if stats.name == name {
            let snap = stats.samples.snapshot();
            if let Some(&m) = snap.iter().max() {
                snapshot_max = snapshot_max.max(m);
            }
        }
    });
    assert!(
        snapshot_max >= 64,
        "reservoir must contain the largest cap_inspect sample (64), got {snapshot_max}"
    );
}

// ── test 5: wrap_from construction_count one ──────────────────────────────────

/// Simple wrap_from → creation_count == 1.
#[test]
fn wrap_from_construction_count_one() {
    let name = "creation_count_tests:wrap_from_count_one";
    let before = creation_count(name);
    let inner: Vec<u32> = Vec::with_capacity(64);
    let _t = TrackedVec::wrap_from(inner, name, file!(), line!(), column!());
    assert_eq!(
        creation_count(name) - before,
        1,
        "wrap_from must increment creation_count exactly once"
    );
}

// ── test 6: with_capacity_named construction_count one ───────────────────────

/// Simple with_capacity_named → creation_count == 1.
#[test]
fn with_capacity_named_construction_count_one() {
    let name = "creation_count_tests:with_capacity_named_count_one";
    let before = creation_count(name);
    let _t: TrackedVecDeque<u32> =
        TrackedVecDeque::with_capacity_named(16, name, file!(), line!(), column!());
    assert_eq!(
        creation_count(name) - before,
        1,
        "with_capacity_named must increment creation_count exactly once"
    );
}
