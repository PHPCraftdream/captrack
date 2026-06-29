// captrack-pgo-lint per-type integration test: scc::HashMap — hasher injection.
//
// CAPTRACK_PGO_HASHER=fx → suggests with_capacity_and_hasher(64, ::fxhash::FxBuildHasher::default()).
// Third-party type: requires --extern scc flag passed by test harness.

#![allow(unused_variables)]

fn main() {
    let m = scc::HashMap::<u32, u32>::with_capacity(0);
    let _ = m.len();
}
