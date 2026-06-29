// captrack-pgo-lint per-type integration test: BTreeMap — warning-only (no with_capacity).
//
// BTreeMap has no with_capacity → lint emits "consider with_capacity" warning only,
// no rustfix suggestion.

#![allow(unused_variables)]

use std::collections::BTreeMap;

fn main() {
    let m = BTreeMap::<u32, u32>::new();
    let _ = m.len();
}
