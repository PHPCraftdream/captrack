// captrack-pgo-lint per-type integration test example: scc::HashSet — capacity rewrite.
//
// Compiled as a Cargo example so `scc` dep is available.
// Profile provides p95=60 → next_pow2(60) = 64.

#![allow(unused_variables)]

fn main() {
    let s = scc::HashSet::<u32>::with_capacity(0);
    let _ = s.len();
}
