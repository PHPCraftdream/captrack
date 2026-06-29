// captrack-pgo-lint per-type integration test example: IndexMap — hasher injection.
//
// CAPTRACK_PGO_HASHER=fx set → suggests with_capacity_and_hasher(64, ::fxhash::FxBuildHasher::default()).
// No type ascription → hasher injection is safe.

#![allow(unused_variables)]

fn main() {
    let m = indexmap::IndexMap::<u32, u32>::with_capacity(0);
    let _ = m.len();
}
