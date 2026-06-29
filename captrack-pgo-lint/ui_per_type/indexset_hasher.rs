// captrack-pgo-lint per-type integration test: IndexSet — hasher injection.
//
// CAPTRACK_PGO_HASHER=fx → suggests with_capacity_and_hasher(64, ::fxhash::FxBuildHasher::default()).
// Third-party type: requires --extern indexmap flag passed by test harness.

#![allow(unused_variables)]

fn main() {
    let s = indexmap::IndexSet::<u32>::with_capacity(0);
    let _ = s.len();
}
