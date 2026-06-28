// captrack-pgo-lint M6 UI test fixture — CAPTRACK_PGO_INSTRUMENT lint.
//
// Sites that should be instrumented (all bare std constructors NOT already
// wrapped and NOT macro-expanded):
//   - Vec::new()
//   - Vec::with_capacity(8)
//   - HashMap::new()
//   - BTreeMap::new()
//
// Sites that should be skipped:
//   - vec![] — macro-expanded, caught by span.from_expansion()

#![allow(unused_variables)]

use std::collections::{BTreeMap, HashMap};

fn main() {
    // INSTRUMENT: Vec::new() → TrackedVec::with_capacity_named
    let _v: Vec<u8> = Vec::new();

    // INSTRUMENT: Vec::with_capacity(8) → TrackedVec::with_capacity_named(8, ...)
    let _v2: Vec<u8> = Vec::with_capacity(8);

    // INSTRUMENT: HashMap::new() → TrackedHashMap::with_capacity_named
    let _m: HashMap<u8, u8> = HashMap::new();

    // INSTRUMENT: BTreeMap::new() → TrackedBTreeMap::new_named
    let _b: BTreeMap<u8, u8> = BTreeMap::new();

    // SKIP: vec![] is macro-expanded — from_expansion() guard fires.
    let _macro_vec: Vec<u8> = vec![];
}
