// captrack-pgo-lint M2 UI test fixture.
//
// The test profile (generated at test time) is built by scanning this file
// for Vec:: constructor patterns, so line numbers do not need to be hardcoded.
// Matched sites produce CAPTRACK_PGO_CAPACITY warnings; unmatched sites are silent.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

fn main() {
    // Matched by the test profile (Vec::new on this line).
    let _v: Vec<u8> = Vec::new();

    // Not in profile — no warning expected.
    let _m: HashMap<u8, u8> = HashMap::new();

    // Not in profile — VecDeque.
    let _d: std::collections::VecDeque<u8> = std::collections::VecDeque::new();

    // Not in profile — BTreeMap.
    let _b: BTreeMap<u8, u8> = BTreeMap::new();

    // Not in profile — BTreeSet.
    let _s: BTreeSet<u8> = BTreeSet::new();

    // Not in profile — HashSet.
    let _h: HashSet<u8> = HashSet::new();

    // Matched by the test profile (Vec::with_capacity on this line).
    let _v2: Vec<u8> = Vec::with_capacity(10);
}
