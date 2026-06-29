// captrack-pgo-lint per-type integration test: scc::HashSet — capacity rewrite.
//
// Third-party type: requires --extern scc flag passed by test harness.
// Profile provides p95=60 → next_pow2(60) = 64.

#![allow(unused_variables)]

fn main() {
    let s = scc::HashSet::<u32>::with_capacity(0);
    let _ = s.len();
}
