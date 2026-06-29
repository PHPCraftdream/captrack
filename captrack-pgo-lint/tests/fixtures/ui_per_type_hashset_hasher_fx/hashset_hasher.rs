// captrack-pgo-lint per-type integration test: HashSet — hasher injection.
//
// CAPTRACK_PGO_HASHER=fx set → suggests with_capacity_and_hasher(64, ::fxhash::FxBuildHasher::default()).
// No type ascription → hasher injection is safe.

#![allow(unused_variables)]

use std::collections::HashSet;

fn main() {
    let s = HashSet::<u32>::with_capacity(0);
    let _ = s.len();
}
