// captrack-pgo-lint Phase K UI test fixture — CAPTRACK_PGO_INSTRUMENT lint.
//
// Phase K: the lint now wraps any constructor expression with wrap_from
// instead of replacing it.  All sites use unannotated let bindings so
// the is_safe_instrument_context data-flow guard allows the rewrite.
//
// Sites that should be instrumented:
//   - Vec::new()          → TrackedVec::wrap_from(Vec::new(), ...)
//   - Vec::with_capacity  → TrackedVec::wrap_from(Vec::with_capacity(8), ...)
//   - HashMap::new()      → TrackedHashMap::wrap_from(HashMap::new(), ...)
//   - BTreeMap::new()     → TrackedBTreeMap::wrap_from(BTreeMap::new(), ...)
//
// The auto-label in the suggestion uses build_auto_label which normalises
// path separators to forward slashes.  On Windows this means the path won't
// match compiletest's $DIR substitution (which uses OS-native backslashes).
// We use normalize-stderr to strip the absolute path prefix in the suggestion
// string so the test is machine-agnostic.
// normalize-stderr-test: "auto:.*instrument.rs" -> "auto:$$DIR/instrument.rs"

#![allow(unused_variables)]

use std::collections::{BTreeMap, HashMap};

fn main() {
    // INSTRUMENT: Vec::new() → TrackedVec::wrap_from(Vec::new(), ...)
    let _v = Vec::<u8>::new();

    // INSTRUMENT: Vec::with_capacity(8) → TrackedVec::wrap_from(Vec::with_capacity(8), ...)
    let _v2 = Vec::<u8>::with_capacity(8);

    // INSTRUMENT: HashMap::new() → TrackedHashMap::wrap_from(HashMap::new(), ...)
    let _m = HashMap::<u8, u8>::new();

    // INSTRUMENT: BTreeMap::new() → TrackedBTreeMap::wrap_from(BTreeMap::new(), ...)
    let _b = BTreeMap::<u8, u8>::new();
}
