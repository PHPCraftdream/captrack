// captrack-pgo-lint per-type integration test: DashMap — hasher injection.
//
// CAPTRACK_PGO_HASHER=fx → suggests with_capacity_and_hasher(64, ::fxhash::FxBuildHasher::default()).
// Third-party type: requires --extern dashmap flag passed by test harness.

#![allow(unused_variables)]

fn main() {
    let m = dashmap::DashMap::<u32, u32>::with_capacity(0);
    let _ = m.len();
}
