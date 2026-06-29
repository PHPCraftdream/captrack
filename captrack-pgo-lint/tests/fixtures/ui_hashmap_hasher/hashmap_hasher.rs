// captrack-pgo-lint per-type integration test: HashMap — hasher injection.
//
// CAPTRACK_PGO_HASHER=fx set → suggests with_capacity_and_hasher(64, ::fxhash::FxBuildHasher::default()).
// No type ascription → hasher injection is safe.

#![allow(unused_variables)]

use std::collections::HashMap;

fn main() {
    let m = HashMap::<u32, u32>::with_capacity(0);
    let _ = m.len();
}
