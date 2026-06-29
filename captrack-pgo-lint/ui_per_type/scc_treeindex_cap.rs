// captrack-pgo-lint per-type integration test: scc::TreeIndex — warning-only.
//
// scc::TreeIndex has no with_capacity → lint emits "consider with_capacity" warning only.
// Third-party type: requires --extern scc flag passed by test harness.

#![allow(unused_variables)]

fn main() {
    let t = scc::TreeIndex::<u32, u32>::new();
    let _ = t.len();
}
