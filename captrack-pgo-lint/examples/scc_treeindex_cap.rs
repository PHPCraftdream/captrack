// captrack-pgo-lint per-type integration test example: scc::TreeIndex — warning-only.
//
// scc::TreeIndex has no with_capacity → lint emits "consider with_capacity" warning only.
// Compiled as a Cargo example so `scc` dep is available.

#![allow(unused_variables)]

fn main() {
    let t = scc::TreeIndex::<u32, u32>::new();
    let _ = t.len();
}
