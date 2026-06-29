// captrack-pgo-lint per-type integration test: BTreeSet — warning-only (no with_capacity).
//
// BTreeSet has no with_capacity → lint emits "consider with_capacity" warning only,
// no rustfix suggestion.

#![allow(unused_variables)]

use std::collections::BTreeSet;

fn main() {
    let s = BTreeSet::<u32>::new();
    let _ = s.len();
}
